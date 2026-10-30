"""Recall latency, CLI and warm daemon, per stage.

    python3 -I benchmarks/recall_latency.py --bin <engram> --daemon <engramd> \
        --store <dir> [--rounds 2] [--label name] [--surface cli,daemon]

<dir> must be a throwaway COPY of a store (recall records usage events in
it). Its config.toml supplies the provider keys, exactly as for a user.
Prints p50/p95 of wall time per surface and, when the binary logs them,
of each `engram::timing` stage. Never prints what recall returned.
"""
import argparse, json, os, re, signal, socket, statistics, subprocess, sys, time, urllib.request

# Neutral questions an assistant might ask a personal memory store.
QUERIES = [
    "what did we decide about pricing",
    "competitor intel",
    "when is the next investor meeting",
    "which supplements am I taking",
    "who introduced me to the new advisor",
    "what is the app store submission status",
    "travel plans for next month",
    "what did the accountant say about taxes",
    "favourite restaurant recommendations",
    "server and hosting costs",
    "what are the open hiring decisions",
    "notes from the last board update",
    "which domain registrar do we use",
    "who is the contact at the bank",
    "what was the feedback on the landing page",
    "marketing plan for the launch",
    "what did I promise to send this week",
    "family birthdays coming up",
    "which API keys were rotated",
    "what is the refund policy",
]

FIELD = re.compile(r'(\w+_ms)=([0-9.]+)')


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return float('nan')
    k = (len(xs) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def stage_lines(text):
    out = []
    for line in text.splitlines():
        if 'engram::timing' in line and 'recall stages' in line:
            out.append({k: float(v) for k, v in FIELD.findall(line)})
    return out


def summarise(label, surface, wall, stages):
    row = {'label': label, 'surface': surface, 'n': len(wall),
           'p50_ms': round(pct(wall, .5)), 'p95_ms': round(pct(wall, .95))}
    keys = sorted({k for s in stages for k in s})
    for k in keys:
        vals = [s[k] for s in stages if k in s]
        row[k] = [round(pct(vals, .5)), round(pct(vals, .95))]
    print(json.dumps(row))
    return row


def run_cli(args, env):
    wall, stages = [], []
    # One unmeasured call: a first open may migrate the copy.
    subprocess.run([args.bin, 'recall', 'warm up', '--json'], env=env, capture_output=True)
    for _ in range(args.rounds):
        for q in QUERIES:
            t = time.perf_counter()
            p = subprocess.run([args.bin, 'recall', q, '--json'], env=env, capture_output=True, text=True)
            wall.append((time.perf_counter() - t) * 1000)
            if p.returncode not in (0, 1):
                print('recall failed', p.returncode, p.stderr[-300:], file=sys.stderr)
            stages += stage_lines(p.stderr)
    return summarise(args.label, 'cli', wall, stages)


def free_port():
    s = socket.socket()
    s.bind(('127.0.0.1', 0))
    port = s.getsockname()[1]
    s.close()
    return port


def post(port, body):
    req = urllib.request.Request(f'http://127.0.0.1:{port}/v1/recall', json.dumps(body).encode(),
                                 {'content-type': 'application/json'})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def run_daemon(args, env):
    port = free_port()
    log = open(f'/tmp/lat-daemon-{port}.log', 'w+')
    proc = subprocess.Popen([args.daemon, '--port', str(port), '--store', args.store], env=env,
                            stdout=subprocess.DEVNULL, stderr=log)
    try:
        for _ in range(200):
            try:
                socket.create_connection(('127.0.0.1', port), 0.2).close()
                break
            except OSError:
                time.sleep(0.05)
        time.sleep(args.settle)
        post(port, {'query': 'warm up'})
        wall = []
        for _ in range(args.rounds):
            for q in QUERIES[:args.limit]:
                if args.gap:
                    time.sleep(args.gap)
                t = time.perf_counter()
                body = post(port, {'query': q})
                wall.append((time.perf_counter() - t) * 1000)
                if body.get('status') not in ('success', 'no_results'):
                    print('daemon recall:', body.get('status'), file=sys.stderr)
    finally:
        proc.send_signal(signal.SIGTERM)
        proc.wait(10)
    log.seek(0)
    stages = stage_lines(log.read())[1:]  # drop the warm-up
    return summarise(args.label, 'daemon' + (f'+gap{args.gap}s' if args.gap else ''), wall, stages)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--bin')
    ap.add_argument('--daemon')
    ap.add_argument('--store', required=True)
    ap.add_argument('--rounds', type=int, default=2)
    ap.add_argument('--label', default='')
    ap.add_argument('--surface', default='cli,daemon')
    ap.add_argument('--gap', type=float, default=0.0, help='idle seconds before each daemon call')
    ap.add_argument('--settle', type=float, default=0.0, help='seconds to wait after daemon start')
    ap.add_argument('--limit', type=int, default=len(QUERIES), help='queries per daemon round')
    args = ap.parse_args()
    env = dict(os.environ, ENGRAM_STORE=args.store, NO_COLOR='1',
               RUST_LOG=os.environ.get('RUST_LOG', 'warn,engram::timing=info'))
    if 'cli' in args.surface:
        run_cli(args, env)
    if 'daemon' in args.surface:
        run_daemon(args, env)


if __name__ == '__main__':
    main()
