#!/usr/bin/env python3
"""A/B gate: does a recall change lose judged relevance on real queries?

Runs `engram recall <q> --top-k 5 --json` with two binaries against separate
copies of one store, then asks the shipped JEV relevance question
(crates/engram-llm/src/judge/questions.rs RELEVANCE_QUESTION) about every
returned passage, one request per query. Reports, per build: mean relevant
slots in the top 5 (precision@5, in points), the share of queries with at least
one relevant hit (hit@5), how often a derived passage (claim, wiki page,
entity, relation) sat in the old top 5, and how many queries lost their only
relevant hit.

Queries and the store are the caller's and may be personal: they are read
from paths given on the command line and nothing here writes them anywhere
but --out. The key is TYPESAFE_API_KEY (pass it with akm run) and is never
printed; the embedder key is GEMINI_API_KEY.

Usage:
  akm run --only TYPESAFE_API_KEY=... --only GEMINI_API_KEY=... -- \
    python3 benchmarks/jev/recall_ab_gate.py --old ./engram-old --new ./engram-new \
      --store-db /path/to/snapshot.sqlite --queries q.json --out result.json
"""
import argparse, concurrent.futures as cf, json, os, shutil, subprocess, sys, tempfile, time
import urllib.error, urllib.request

URL = 'https://api.typesafe.ai/v1/systemone'
MODEL = 'jev-1.13.0'  # pinned like crates/engram-llm/src/judge/typesafe.rs
QUESTION = 'Does `passage` contain information that helps answer `query`?'
PASSAGE_CHARS = 2000
DERIVED = {'claim', 'wiki_page', 'wiki', 'entity', 'relation', 'graph'}


def copy_store(db):
    d = tempfile.mkdtemp(prefix='engram-ab-')
    for suffix in ('', '-wal', '-shm'):
        if os.path.exists(db + suffix):
            shutil.copy2(db + suffix, os.path.join(d, 'db.sqlite' + suffix))
    return d


def recall(binary, store, q, extra=()):
    env = dict(os.environ, ENGRAM_STORE=store)
    env.pop('TYPESAFE_API_KEY', None)  # recall's own judge stays off: compare raw retrieval
    p = subprocess.run([binary, 'recall', q, '--top-k', '5', '--json', '--quiet', *extra],
                       env=env, capture_output=True, text=True, timeout=300)
    try:
        doc = json.loads(p.stdout)
    except json.JSONDecodeError:
        return None, (p.stderr or p.stdout)[-300:]
    data = doc.get('data') or {}
    results = data.get('results') if isinstance(data, dict) else data
    out = []
    for r in (results or [])[:5]:
        out.append({'id': str(r.get('id')), 'kind': r.get('kind', 'chunk'),
                    'text': r.get('content') or r.get('text') or ''})
    return out, None


def ask(key, query, passages, retries=4):
    questions = {f'p{i}': {'type': 'noul', 'instructions': {
        'passage': t[:PASSAGE_CHARS], 'question': QUESTION}} for i, t in enumerate(passages)}
    body = json.dumps({'model': MODEL, 'state': {'query': query}, 'questions': questions}).encode()
    for attempt in range(retries):
        try:
            req = urllib.request.Request(URL, body, {'Authorization': 'Bearer ' + key,
                                                     'Content-Type': 'application/json'})
            with urllib.request.urlopen(req, timeout=60) as r:
                ans = json.load(r)['answers']
            return [ans[f'p{i}']['noul'] for i in range(len(passages))]
        except (urllib.error.URLError, TimeoutError, KeyError) as e:
            if attempt == retries - 1:
                raise RuntimeError(f'judge failed: {type(e).__name__}') from None
            time.sleep(2 ** attempt)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--old', required=True)
    ap.add_argument('--new', required=True)
    ap.add_argument('--store-db', required=True)
    ap.add_argument('--queries', required=True)
    ap.add_argument('--out', required=True)
    ap.add_argument('--old-args', default='', help='extra recall args for the old side, space separated')
    ap.add_argument('--new-args', default='', help='extra recall args for the new side, e.g. "--mode raw"')
    ap.add_argument('--threshold', type=float, default=0.5)
    a = ap.parse_args()
    key = os.environ['TYPESAFE_API_KEY']
    queries = json.load(open(a.queries))
    stores = {'old': copy_store(a.store_db), 'new': copy_store(a.store_db)}
    bins = {'old': a.old, 'new': a.new}
    extra = {'old': a.old_args.split(), 'new': a.new_args.split()}
    rows = []
    for i, q in enumerate(queries):
        row = {'i': i}
        for b in ('old', 'new'):
            res, err = recall(bins[b], stores[b], q, extra[b])
            row[b] = res or []
            if err:
                row[b + '_error'] = err
        rows.append(row)
        print(f'recall {i + 1}/{len(queries)}', file=sys.stderr)

    def judge(row):
        texts = sorted({p['text'] for b in ('old', 'new') for p in row[b] if p['text']})
        scores = dict(zip(texts, ask(key, queries[row['i']], texts))) if texts else {}
        for b in ('old', 'new'):
            for p in row[b]:
                p['p'] = scores.get(p['text'], 0.0)
        return row

    with cf.ThreadPoolExecutor(8) as ex:
        rows = list(ex.map(judge, rows))

    def summarize(t):
        s = {}
        for b in ('old', 'new'):
            rel = [sum(p['p'] >= t for p in r[b]) for r in rows]
            s[b] = {'precision_at5_pts': round(100 * sum(rel) / (5 * len(rows)), 2),
                    'hit_at5_pct': round(100 * sum(x > 0 for x in rel) / len(rows), 2),
                    'mean_relevant_in_top5': round(sum(rel) / len(rows), 3)}
        lost_only = 0
        for r in rows:
            old_rel = {p['text'] for p in r['old'] if p['p'] >= t}
            new_rel = {p['text'] for p in r['new'] if p['p'] >= t}
            if old_rel and not new_rel:
                lost_only += 1
        s['queries_lost_only_relevant_hit'] = lost_only
        s['queries_with_derived_in_old_top5'] = sum(
            any(p['kind'] in DERIVED for p in r['old']) for r in rows)
        s['derived_relevant_in_old_top5'] = sum(
            p['kind'] in DERIVED and p['p'] >= t for r in rows for p in r['old'])
        return s

    report = {'queries': len(rows), 'model': MODEL, 'threshold': a.threshold,
              'at_threshold': summarize(a.threshold), 'at_0.15': summarize(0.15),
              'errors': sum(('old_error' in r) + ('new_error' in r) for r in rows),
              'rows': rows}
    json.dump(report, open(a.out, 'w'), indent=1)
    print(json.dumps({k: v for k, v in report.items() if k != 'rows'}, indent=1))
    for s in stores.values():
        shutil.rmtree(s, ignore_errors=True)


if __name__ == '__main__':
    main()
