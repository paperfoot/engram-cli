#!/usr/bin/env python3
"""Exploratory probe: can a calibrated judge replace compile's word-overlap grounding gate?

Ground truth is SciFact's human labels (Wadden et al. 2020): each claim is paired
with an abstract that SUPPORTs it, CONTRADICTs it, or was cited without evidence
(NEI). "Supported" is the positive class; CONTRADICT and NEI are negatives.

Compares three gates on the same pairs:
  heuristic  - a line-for-line port of compile.rs `claim_grounded_in_chunk`
  jev        - TypeSafe JEV noul per claim, all claims of one abstract in one request
  num+jev    - compile's numeric-token rule as a hard gate, then JEV

This is the design probe. The shipping eval runs the Rust questions module
(`engram bench grounding`), so question text cannot drift from what ships.

Usage:
  curl -L https://scifact.s3-us-west-2.amazonaws.com/release/latest/data.tar.gz | tar xz -C data/scifact --strip-components=1
  python3 benchmarks/jev/scifact_grounding_probe.py build
  python3 benchmarks/jev/scifact_grounding_probe.py probe plain     # needs the TypeSafe key in Keychain
  JEV_MODEL=jev-1.14.0 python3 benchmarks/jev/scifact_grounding_probe.py probe plain
  python3 benchmarks/jev/scifact_grounding_probe.py score plain@jev-1.13.0 plain@jev-1.14.0 --save benchmarks/jev-grounding-probe-scifact-<ts>.json

The key is read from the macOS Keychain item `typesafe-jev-api-key` (or
TYPESAFE_API_KEY) and never printed. SciFact is CC-BY-NC: data stays in the
gitignored data/scifact/; only aggregate numbers are committed.
"""
import collections, concurrent.futures as cf, json, os, re, subprocess, sys, time, urllib.error, urllib.request

DATA = os.environ.get('SCIFACT_DIR', 'data/scifact')
URL = 'https://api.typesafe.ai/v1/systemone'
# Pinned like typesafe.rs; set JEV_MODEL to compare versions (results land in probe-<variant>@<model>.json).
MODEL = os.environ.get('JEV_MODEL', 'jev-1.13.0')


# ---- compile.rs heuristic, ported verbatim ---------------------------------

def numeric_tokens(text):
    out, cur = [], ''
    for c in text:
        if (c.isascii() and c.isdigit()) or (c == '.' and cur):
            cur += c
        else:
            if any(ch.isdigit() for ch in cur):
                out.append(cur.strip('.'))
            cur = ''
    if any(ch.isdigit() for ch in cur):
        out.append(cur.strip('.'))
    return out


def word_set(text):
    return {w.lower() for w in re.split(r'[^A-Za-z0-9]', text) if w}


def numbers_ok(claim, chunk):
    chunk_nums = numeric_tokens(chunk.lower())
    return all(len(n) < 2 or n in chunk_nums for n in numeric_tokens(claim.lower()))


def heuristic(claim, chunk):
    if not numbers_ok(claim, chunk):
        return False
    chunk_words = {w for w in word_set(chunk.lower()) if len(w) >= 4}
    claim_words = [w for w in word_set(claim.lower()) if len(w) >= 4]
    if not claim_words:
        return True
    return sum(w in chunk_words for w in claim_words) / len(claim_words) >= 0.5


# ---- data -------------------------------------------------------------------

def build():
    corpus = {}
    for line in open(f'{DATA}/corpus.jsonl'):
        d = json.loads(line)
        corpus[d['doc_id']] = d
    pairs = []
    for split in ['train', 'dev']:
        for line in open(f'{DATA}/claims_{split}.jsonl'):
            c = json.loads(line)
            if c['evidence']:
                for doc, evs in c['evidence'].items():
                    (label,) = {e['label'] for e in evs}
                    pairs.append({'id': f"{split}-{c['id']}-{doc}", 'doc_id': int(doc), 'claim': c['claim'], 'label': label})
            else:
                doc = c['cited_doc_ids'][0]
                pairs.append({'id': f"{split}-{c['id']}-{doc}", 'doc_id': doc, 'claim': c['claim'], 'label': 'NEI'})
    with open(f'{DATA}/grounding-scifact-pairs.jsonl', 'w') as f:
        for p in pairs:
            f.write(json.dumps(p) + '\n')
    with open(f'{DATA}/grounding-scifact-docs.jsonl', 'w') as f:
        for d in sorted({p['doc_id'] for p in pairs}):
            c = corpus[d]
            f.write(json.dumps({'doc_id': d, 'title': c['title'], 'text': ' '.join(c['abstract'])}) + '\n')
    print(len(pairs), collections.Counter(p['label'] for p in pairs))


def load():
    pairs = [json.loads(l) for l in open(f'{DATA}/grounding-scifact-pairs.jsonl')]
    docs = {}
    for line in open(f'{DATA}/grounding-scifact-docs.jsonl'):
        d = json.loads(line)
        docs[d['doc_id']] = d['title'] + '. ' + d['text']
    return pairs, docs


# ---- JEV ----------------------------------------------------------------------

VARIANTS = {
    'plain': lambda claim: {
        'type': 'noul',
        'instructions': {'claim': claim, 'question': 'Does `passage` directly support `claim`?'},
        'criteria': {'true': 'The passage states or directly implies the claim.',
                     'false': 'The passage does not state the claim, contradicts it, or only discusses the topic.'},
    },
    'strict': lambda claim: {
        'type': 'noul',
        'instructions': {'claim': claim, 'question': 'Does `passage` directly support `claim`, with the same direction of effect, the same subject, the same population or species, and the same strength?'},
        'criteria': {
            'true': 'The passage states or directly implies the claim: same direction (increase vs decrease, more vs less), same subject and object, same population or species, and no stronger certainty than the passage (a hedge like "may" or "is associated with" is not turned into a definite or causal statement). A faithful paraphrase counts.',
            'false': "The passage contradicts the claim, reverses its direction, drops a negation or hedge, swaps subject and object, reports it for a different population or species (for example mice instead of humans), turns an association into causation, attributes a cited prior study's finding to this work, or simply does not state the claim.",
        },
    },
}


def api_key():
    key = os.environ.get('TYPESAFE_API_KEY')
    if key:
        return key
    return subprocess.check_output(['security', 'find-generic-password', '-s', 'typesafe-jev-api-key', '-w']).decode().strip()


def ask(key, state, questions, retries=4):
    body = json.dumps({'model': MODEL, 'state': state, 'questions': questions}).encode()
    err = None
    for i in range(retries):
        t = time.time()
        try:
            req = urllib.request.Request(URL, body, {'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
            d = json.load(urllib.request.urlopen(req, timeout=30))
            d['_latency'] = time.time() - t
            return d
        except urllib.error.HTTPError as e:
            err = f'{e.code} {e.read().decode()[:300]}'
            if e.code in (429, 500, 502, 503, 504):
                time.sleep(2 ** i)
                continue
            return {'error': err}
        except Exception as e:  # network
            err = str(e)
            time.sleep(2 ** i)
    return {'error': err}


def probe(variant, workers=8):
    pairs, docs = load()
    key = api_key()
    by_doc = collections.defaultdict(list)
    for p in pairs:
        by_doc[p['doc_id']].append(p)

    def job(doc_id):
        qs = {p['id']: VARIANTS[variant](p['claim']) for p in by_doc[doc_id]}
        return doc_id, ask(key, {'passage': docs[doc_id]}, qs)

    scores, lat, toks = {}, [], 0
    with cf.ThreadPoolExecutor(workers) as ex:
        for doc_id, resp in ex.map(job, list(by_doc)):
            if 'error' in resp:
                print('ERR', doc_id, resp['error'], file=sys.stderr)
                continue
            lat.append(resp['_latency'])
            toks += resp.get('usage', {}).get('input_tokens', 0)
            for qid, a in resp['answers'].items():
                scores[qid] = a.get('noul')
    lat.sort()
    out = {'variant': variant, 'model': MODEL, 'scores': scores, 'requests': len(lat),
           'latency_p50_s': lat[len(lat) // 2], 'latency_p95_s': lat[int(len(lat) * .95)], 'input_tokens': toks}
    json.dump(out, open(f'{DATA}/probe-{variant}@{MODEL}.json', 'w'))
    print(variant, 'requests', len(lat), 'p50 %.2fs p95 %.2fs' % (out['latency_p50_s'], out['latency_p95_s']),
          'input tokens', toks, 'usd %.4f' % (toks * 0.042e-6))


# ---- scoring ----------------------------------------------------------------

def metrics(pred, pairs):
    n = collections.Counter(p['label'] for p in pairs)
    tp = sum(1 for p, x in zip(pairs, pred) if x and p['label'] == 'SUPPORT')
    fp = sum(1 for p, x in zip(pairs, pred) if x and p['label'] != 'SUPPORT')
    fn = sum(1 for p, x in zip(pairs, pred) if not x and p['label'] == 'SUPPORT')
    return {
        'precision_supported': round(tp / max(tp + fp, 1), 4),
        'recall_supported': round(tp / max(tp + fn, 1), 4),
        'contradict_admitted': round(sum(1 for p, x in zip(pairs, pred) if x and p['label'] == 'CONTRADICT') / n['CONTRADICT'], 4),
        'nei_admitted': round(sum(1 for p, x in zip(pairs, pred) if x and p['label'] == 'NEI') / n['NEI'], 4),
    }


def auc(scores, labels):
    pos = [s for s, l in zip(scores, labels) if l]
    neg = [s for s, l in zip(scores, labels) if not l]
    return sum((p > q) + 0.5 * (p == q) for p in pos for q in neg) / (len(pos) * len(neg))


def score(variants, save=None):
    pairs, docs = load()
    report = {'suite': 'jev-grounding-probe-scifact', 'pairs': len(pairs),
              'labels': dict(collections.Counter(p['label'] for p in pairs)),
              'heuristic': metrics([heuristic(p['claim'], docs[p['doc_id']]) for p in pairs], pairs), 'jev': {}}
    print('heuristic', report['heuristic'])
    for v in variants:
        r = json.load(open(f'{DATA}/probe-{v}.json'))
        s = [r['scores'].get(p['id']) for p in pairs]
        missing = sum(x is None for x in s)
        s = [x or 0.0 for x in s]
        entry = {'auc': round(auc(s, [p['label'] == 'SUPPORT' for p in pairs]), 4), 'missing': missing,
                 'requests': r['requests'], 'input_tokens': r['input_tokens'],
                 'usd': round(r['input_tokens'] * 0.042e-6, 4),
                 'latency_p50_s': round(r['latency_p50_s'], 2), 'latency_p95_s': round(r['latency_p95_s'], 2), 'thresholds': {}}
        for t in [0.3, 0.5, 0.7, 0.8, 0.9, 0.95]:
            entry['thresholds'][str(t)] = {
                'jev': metrics([x >= t for x in s], pairs),
                'num+jev': metrics([x >= t and numbers_ok(p['claim'], docs[p['doc_id']]) for x, p in zip(s, pairs)], pairs),
            }
        report['jev'][v] = entry
        print(v, 'auc', entry['auc'], entry['thresholds']['0.5'])
    if save:
        json.dump(report, open(save, 'w'), indent=2)
        print('saved', save)


if __name__ == '__main__':
    cmd = sys.argv[1]
    if cmd == 'build':
        build()
    elif cmd == 'probe':
        probe(sys.argv[2])
    elif cmd == 'score':
        args = sys.argv[2:]
        save = None
        if '--save' in args:
            i = args.index('--save')
            save = args[i + 1]
            args = args[:i] + args[i + 2:]
        score(args, save)
