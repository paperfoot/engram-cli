"""LongMemEval evidence recall through the FULL default recall pipeline.

`longmemeval_cli.py recall` pins retrieval (no reranker, no relevance judge)
so its arms differ only in what recall prints. This one does the opposite:
nothing is pinned, so Gemini embedding, Cohere rerank and the JEV relevance
judge all run exactly as a user's recall runs them, and it measures what
comes back. Reuses the frozen per-question stores `longmemeval_cli.py build`
made, and writes `runs/<cond>/recall.json` in the same shape, so
`longmemeval_cli.py answer --cond <cond>` scores answers on these recalls.

  recall  --bin B --cond C [--env K=V ...]   one recall per question
  report  --conds A,B[,...]                  evidence recall@5/@10, A/A deltas
  accuracy --conds A,B[,...]                 answer accuracy over EVERY question
                                             (after `longmemeval_cli.py answer`)

Evidence recall@k: the share of a question's gold evidence sessions
(LongMemEval `answer_session_ids`) whose memory is among the first k results.
Keys come from the environment: GEMINI_API_KEY, COHERE_API_KEY,
TYPESAFE_API_KEY.
"""
import argparse, concurrent.futures as cf, json, os, shutil, sqlite3, statistics, subprocess, sys, time

WORK = os.environ.get('LME_WORK', '/tmp/lme')


def jload(p):
    return json.load(open(p))


def jdump(p, obj):
    os.makedirs(os.path.dirname(p), exist_ok=True)
    json.dump(obj, open(p + '.tmp', 'w'))
    os.replace(p + '.tmp', p)


def session_of(store, q):
    """memory id -> session id, by exact text, else by event time."""
    con = sqlite3.connect(f'{store}/db.sqlite')
    rows = con.execute('select id, content, event_time from memories where deleted = 0').fetchall()
    con.close()
    by_text = {s['text']: s['sid'] for s in q['sessions']}
    by_time = {s['date'][:16]: s['sid'] for s in q['sessions']}
    out = {}
    for mid, content, event_time in rows:
        sid = by_text.get(content) or by_time.get((event_time or '')[:16])
        if sid:
            out[mid] = sid
    return out


def recall_one(binary, cond, q, extra_env, top_k):
    src = f'{WORK}/stores/{q["qid"]}'
    if not os.path.exists(f'{src}/.built'):
        return {'qid': q['qid'], 'failure': 'store not built'}
    dst = f'{WORK}/runs/{cond}/stores/{q["qid"]}'
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    env = dict(os.environ, ENGRAM_STORE=dst, NO_COLOR='1', **extra_env)
    argv = [binary, 'recall', q['question'], '--json', '--top-k', str(top_k)]
    t = time.time()
    p = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=300)
    secs = time.time() - t
    row = {'qid': q['qid'], 'code': p.returncode, 'stdout': p.stdout, 'bytes': len(p.stdout.encode()),
           'secs': round(secs, 3)}
    try:
        data = json.loads(p.stdout)['data']
        sids = session_of(dst, q)
        results = data.get('results', [])
        row['result_ids'] = [r['id'] for r in results]
        row['sessions'] = [sids.get(r.get('memory_id')) for r in results]
        row['relevance'] = [r.get('relevance') for r in results]
        row['judge'] = (data.get('metadata') or {}).get('relevance')
        env_meta = json.loads(p.stdout).get('metadata') or {}
        row['judge'] = env_meta.get('relevance', row['judge'])
        row['reranker'] = env_meta.get('reranker')
        row['status'] = data.get('status')
    except Exception:
        row['failure'] = f'recall exit {p.returncode}: {p.stderr[-200:]}'
    shutil.rmtree(dst, ignore_errors=True)
    return row


def recall(args):
    qs = jload(f'{WORK}/questions.json')
    extra = dict(kv.split('=', 1) for kv in args.env)
    with cf.ThreadPoolExecutor(args.workers) as ex:
        rows = list(ex.map(lambda q: recall_one(args.bin, args.cond, q, extra, args.top_k), qs))
    jdump(f'{WORK}/runs/{args.cond}/recall.json', rows)
    jdump(f'{WORK}/runs/{args.cond}/meta.json', {'binary': args.bin, 'env': extra, 'top_k': args.top_k,
                                                 'pipeline': 'full default'})
    fails = [r for r in rows if r.get('failure')]
    errs = sum(1 for r in rows if (r.get('judge') or {}).get('error'))
    print(args.cond, 'recalls', len(rows), 'failures', len(fails), 'judge errors', errs,
          'rerankers', sorted({str(r.get('reranker')) for r in rows}))


def gold(q):
    return {s['sid'] for s in q['sessions'] if s['evidence']}


def at_k(row, q, k):
    g = gold(q)
    got = {s for s in (row.get('sessions') or [])[:k] if s}
    return len(g & got) / len(g) if g else None


def report(args):
    qs = {q['qid']: q for q in jload(f'{WORK}/questions.json')}
    conds = args.conds.split(',')
    runs = {c: {r['qid']: r for r in jload(f'{WORK}/runs/{c}/recall.json')} for c in conds}
    valid = [qid for qid in qs if all(not runs[c].get(qid, {}).get('failure') and qid in runs[c] for c in conds)]
    print(f'questions {len(qs)}, valid in every arm {len(valid)}')
    for c in conds:
        r5 = [at_k(runs[c][q], qs[q], 5) for q in valid]
        r10 = [at_k(runs[c][q], qs[q], 10) for q in valid]
        all10 = sum(1 for q in valid if at_k(runs[c][q], qs[q], 10) == 1.0)
        secs = [runs[c][q]['secs'] for q in valid]
        n_res = [len(runs[c][q].get('result_ids') or []) for q in valid]
        empty = sum(1 for n in n_res if n == 0)
        print(f'{c}: recall@5 {statistics.mean(r5):.4f}  recall@10 {statistics.mean(r10):.4f}  '
              f'all-gold@10 {all10}/{len(valid)}  mean results {statistics.mean(n_res):.2f}  '
              f'empty {empty}  wall p50 {statistics.median(secs):.2f}s')
    ref = conds[0]
    for c in conds[1:]:
        same = sum(1 for q in valid if runs[c][q].get('result_ids') == runs[ref][q].get('result_ids'))
        same_sess = sum(1 for q in valid if runs[c][q].get('sessions') == runs[ref][q].get('sessions'))
        d10 = [at_k(runs[c][q], qs[q], 10) - at_k(runs[ref][q], qs[q], 10) for q in valid]
        d5 = [at_k(runs[c][q], qs[q], 5) - at_k(runs[ref][q], qs[q], 5) for q in valid]
        print(f'{c} vs {ref}: identical result ids {same}/{len(valid)}, identical session lists '
              f'{same_sess}/{len(valid)}, @5 better/worse {sum(d > 0 for d in d5)}/{sum(d < 0 for d in d5)}, '
              f'@10 better/worse {sum(d > 0 for d in d10)}/{sum(d < 0 for d in d10)}')


def accuracy(args):
    """Answer accuracy per arm over every question each arm has a verdict for.

    `longmemeval_cli.py report` compares only questions whose retrieval was
    identical (its arms differ in presentation); here retrieval is what
    differs, so every question counts."""
    import math
    conds = args.conds.split(',')
    verdicts = {c: {v['qid']: v for v in jload(f'{WORK}/runs/{c}/verdicts.json')} for c in conds}
    common = [q for q in verdicts[conds[0]] if all('correct' in verdicts[c].get(q, {}) for c in conds)]
    print(f'questions with a verdict in every arm: {len(common)}')
    for c in conds:
        k = sum(verdicts[c][q]['correct'] for q in common)
        print(f'{c}: {k}/{len(common)} = {100 * k / max(1, len(common)):.1f}%')

    def mcnemar(b, c):
        n = b + c
        return 1.0 if n == 0 else min(1.0, 2 * sum(math.comb(n, i) for i in range(min(b, c) + 1)) / 2 ** n)

    for a in conds:
        for b in conds:
            if a >= b:
                continue
            g = sum(1 for q in common if verdicts[b][q]['correct'] and not verdicts[a][q]['correct'])
            l = sum(1 for q in common if verdicts[a][q]['correct'] and not verdicts[b][q]['correct'])
            print(f'{b} vs {a}: +{g} / -{l}, exact McNemar p={mcnemar(g, l):.3f}')


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest='cmd', required=True)
    p = sub.add_parser('recall')
    p.add_argument('--bin', required=True)
    p.add_argument('--cond', required=True)
    p.add_argument('--env', action='append', default=[])
    p.add_argument('--top-k', type=int, default=10)
    p.add_argument('--workers', type=int, default=8)
    p = sub.add_parser('report')
    p.add_argument('--conds', required=True)
    p = sub.add_parser('accuracy')
    p.add_argument('--conds', required=True)
    args = ap.parse_args()
    {'recall': recall, 'report': report, 'accuracy': accuracy}[args.cmd](args)


if __name__ == '__main__':
    main()
