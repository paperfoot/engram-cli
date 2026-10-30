#!/usr/bin/env python3
"""LongMemEval through the real `engram` binary: measures exactly what an agent reads.

The Rust QA bench builds its own context (`[session i — sid]`) and seeds every
memory without an event time, so it cannot measure a change to what `engram
recall` prints. This harness shells out to `engram` exactly as an agent does:

  prepare   questions from the S split (one record = one timeline: question
            date, session dates, sessions); evidence = the oracle's sessions,
            plus distractor sessions from the same record that precede the
            question. Every session must precede its question.
  build     one isolated store per question: `engram remember` each session
            in date order with --event-time, fact extraction on and
            --autonomy full for every type. The store is checkpointed and
            integrity-checked, and fingerprinted (binary, inputs, settings).
  recall    copy each frozen store and run `<binary> recall "<q>" --json` with
            retrieval pinned deterministic (relevance judge off, no reranker),
            so arms differ only in what recall PRINTS. Result ids and order are
            compared with the reference arm; a mismatch is reported.
  answer    one answer model reads the question date, the question and the
            recall stdout verbatim; truncated or failed calls are failures,
            never wrong answers.
  judge     LongMemEval's official prompt templates, read from its
            evaluate_qa.py (parsed, never executed); a model family different
            from the answerer.
  report    accuracy per type with Wilson intervals; paired gained/lost and
            exact McNemar per type against the previous arm (the marginal
            effect of each commit) and against the first; how often the
            present-tense line fired; bytes and prompt tokens.

Keys come from the environment only and are never printed: GEMINI_API_KEY
(embeddings), TYPESAFE_API_KEY (adjudication), OPENROUTER_API_KEY (extraction
inside engram, answerer, judge).
"""
import argparse, ast, collections, concurrent.futures as cf, hashlib, json, math, os, random, shutil
import sqlite3, subprocess, sys, time, urllib.error, urllib.request

DATA = os.environ.get('LME_DATA', os.path.expanduser('~/Projects/engram-cli/data/longmemeval/raw'))
WORK = os.environ.get('LME_WORK', '/tmp/lme')
OFFICIAL = os.environ.get('LME_OFFICIAL_EVAL', f'{WORK}/official/evaluate_qa.py')
OR_URL = 'https://openrouter.ai/api/v1/chat/completions'
ANSWER_MODEL = os.environ.get('LME_ANSWER_MODEL', 'openai/gpt-6.1-terra')
ANSWER_EFFORT = os.environ.get('LME_ANSWER_EFFORT', 'low')
JUDGE_MODEL = os.environ.get('LME_JUDGE_MODEL', 'anthropic/claude-haiku-5.5')
DISTRACTORS = int(os.environ.get('LME_DISTRACTORS', '4'))
REGRESSION_PER_TYPE = int(os.environ.get('LME_REGRESSION_PER_TYPE', '25'))
BUILD_ENV = {'ENGRAM_JUDGE_ADJUDICATE': 'on', 'ENGRAM_JUDGE_RELEVANCE': 'off', 'ENGRAM_RERANK_PROVIDER': 'none'}
RECALL_ENV = {'ENGRAM_JUDGE_RELEVANCE': 'off', 'ENGRAM_RERANK_PROVIDER': 'none'}


def jload(p):
    return json.load(open(p))


def jdump(p, obj):
    os.makedirs(os.path.dirname(p), exist_ok=True)
    tmp = p + '.tmp'
    json.dump(obj, open(tmp, 'w'))
    os.replace(tmp, p)


def sha(data):
    return hashlib.sha256(data if isinstance(data, bytes) else data.encode()).hexdigest()


def file_sha(path):
    return sha(open(path, 'rb').read())


def lme_date(s):
    """'2023/05/25 (Thu) 20:21' -> '2023-05-25T20:21:00Z' (sorts as time)."""
    day, _, hm = s.partition(') ')
    y, m, d = day.split(' ')[0].split('/')
    return f'{y}-{m}-{d}T{hm.strip() or "00:00"}:00Z'


def session_text(turns):
    return '\n'.join(f"{t['role']}: {t['content']}" for t in turns)


# --------------------------------------------------------------------- prepare
def prepare(args):
    oracle = jload(f'{DATA}/oracle.json')
    pick = random.Random(7)
    by_type = collections.defaultdict(list)
    for q in oracle:
        by_type[q['question_type']].append(q)
    chosen = by_type['knowledge-update'] + by_type['temporal-reasoning']
    for t, qs in sorted(by_type.items()):
        if t not in ('knowledge-update', 'temporal-reasoning'):
            chosen += pick.sample(qs, min(REGRESSION_PER_TYPE, len(qs)))
    want = {q['question_id']: q for q in chosen}
    print('loading S split…', file=sys.stderr)
    s_split = {q['question_id']: q for q in jload(f'{DATA}/s.json') if q['question_id'] in want}
    out, problems = [], collections.Counter()
    for qid, oq in want.items():
        sq = s_split.get(qid)
        if sq is None:
            problems['missing_in_S'] += 1
            continue
        qdate = lme_date(sq['question_date'])
        evidence = set(oq['haystack_session_ids'])
        sessions = [{'sid': sid, 'date': lme_date(d), 'text': session_text(t), 'evidence': sid in evidence}
                    for sid, d, t in zip(sq['haystack_session_ids'], sq['haystack_dates'], sq['haystack_sessions'])]
        found = {s['sid'] for s in sessions if s['evidence']}
        if found != evidence:
            problems['evidence_not_in_S'] += 1
            continue
        rng = random.Random(int(sha(qid)[:12], 16))  # per question: the cohort cannot shift it
        # Day granularity: the dataset puts some evidence a few hours after the
        # question on the same day; a later DAY would be a broken timeline.
        pool = [s for s in sessions if not s['evidence'] and s['date'][:10] <= qdate[:10]]
        keep = [s for s in sessions if s['evidence']] + rng.sample(pool, min(DISTRACTORS, len(pool)))
        if any(s['date'][:10] > qdate[:10] for s in keep):
            problems['session_after_question'] += 1
            continue
        keep.sort(key=lambda s: (s['date'], s['sid']))
        out.append({'qid': qid, 'type': oq['question_type'], 'question': sq['question'], 'answer': str(sq['answer']),
                    'question_date': sq['question_date'], 'abstention': qid.endswith('_abs'), 'sessions': keep})
    jdump(f'{WORK}/questions.json', out)
    print(json.dumps(collections.Counter(q['type'] for q in out)), 'questions;',
          sum(len(q['sessions']) for q in out), 'sessions; excluded', dict(problems))


# ----------------------------------------------------------------------- build
def run_engram(binary, store, argv, extra_env, timeout=300):
    env = dict(os.environ, ENGRAM_STORE=store, NO_COLOR='1', **extra_env)
    t = time.time()
    p = subprocess.run([binary, *argv], env=env, capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout, p.stderr, time.time() - t


def build_fingerprint(binary, q):
    return sha(json.dumps({'binary': file_sha(binary), 'question': q, 'env': BUILD_ENV}, sort_keys=True))


def freeze(store):
    """Fold the WAL into the main file and prove the copy source is whole."""
    con = sqlite3.connect(f'{store}/db.sqlite')
    con.execute('PRAGMA wal_checkpoint(TRUNCATE)')
    ok = con.execute('PRAGMA integrity_check').fetchone()[0]
    con.close()
    return ok == 'ok'


def build_one(binary, q):
    store = f'{WORK}/stores/{q["qid"]}'
    fp = build_fingerprint(binary, q)
    marker = f'{store}/.built'
    if os.path.exists(marker) and open(marker).read() == fp:
        return q['qid'], 'cached', None
    shutil.rmtree(store, ignore_errors=True)
    os.makedirs(store)
    log = []
    for s in q['sessions']:
        argv = ['remember', s['text'], '--kb', 'lme', '--event-time', s['date'], '--autonomy', 'full', '--json']
        code, out, err, secs = run_engram(binary, store, argv, BUILD_ENV)
        try:
            data = json.loads(out)['data']
        except Exception:
            return q['qid'], 'failed', {'sid': s['sid'], 'why': 'unparseable output', 'err': err[-300:]}
        entry = {'sid': s['sid'], 'code': code, 'secs': round(secs, 2),
                 'fact_extraction': data.get('fact_extraction'), 'facts_added': data.get('facts_added')}
        log.append(entry)
        if code:
            return q['qid'], 'failed', {**entry, 'err': err[-300:]}
    if not freeze(store):
        return q['qid'], 'failed', {'why': 'integrity_check'}
    jdump(f'{store}/build-log.json', log)
    open(marker, 'w').write(fp)
    return q['qid'], 'built', None


def build(args):
    qs = jload(f'{WORK}/questions.json')
    counts = collections.Counter()
    with cf.ThreadPoolExecutor(args.workers) as ex:
        for qid, status, why in ex.map(lambda q: build_one(args.bin, q), qs):
            counts[status] += 1
            if status == 'failed':
                print('FAILED', qid, why, file=sys.stderr)
    print(dict(counts))


# ---------------------------------------------------------------------- recall
def recall_one(binary, cond, q, top_k):
    src = f'{WORK}/stores/{q["qid"]}'
    if not os.path.exists(f'{src}/.built'):
        return {'qid': q['qid'], 'failure': 'store not built'}
    dst = f'{WORK}/runs/{cond}/stores/{q["qid"]}'
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    argv = ['recall', q['question'], '--json'] + (['--top-k', str(top_k)] if top_k else [])
    code, out, err, secs = run_engram(binary, dst, argv, RECALL_ENV)
    shutil.rmtree(dst, ignore_errors=True)
    row = {'qid': q['qid'], 'code': code, 'stdout': out, 'bytes': len(out.encode()), 'secs': round(secs, 2)}
    try:
        data = json.loads(out)['data']
        row['result_ids'] = [r['id'] for r in data.get('results', [])]
        ctx = data.get('answer_context') or ''
        row['updates_shown'] = ctx.count('<engram-update')
    except Exception:
        row['failure'] = f'recall exit {code}: {err[-200:]}'
    return row


def recall(args):
    qs = jload(f'{WORK}/questions.json')
    with cf.ThreadPoolExecutor(args.workers) as ex:
        rows = list(ex.map(lambda q: recall_one(args.bin, args.cond, q, args.top_k), qs))
    jdump(f'{WORK}/runs/{args.cond}/recall.json', rows)
    jdump(f'{WORK}/runs/{args.cond}/meta.json', {'binary': args.bin, 'binary_sha': file_sha(args.bin),
                                                 'env': RECALL_ENV, 'top_k': args.top_k})
    print(args.cond, 'recalls', len(rows), 'failures', sum(1 for r in rows if r.get('failure')),
          'mean bytes', int(sum(r.get('bytes', 0) for r in rows) / max(1, len(rows))))


# ---------------------------------------------------------------- answer/judge
def chat(model, messages, effort=None, max_tokens=2000):
    key = os.environ['OPENROUTER_API_KEY']
    body = {'model': model, 'messages': messages, 'temperature': 0.0, 'max_tokens': max_tokens,
            'usage': {'include': True}}
    if effort:
        body['reasoning'] = {'effort': effort}
    data = json.dumps(body).encode()
    last = 'retries'
    for i in range(6):
        try:
            req = urllib.request.Request(OR_URL, data, {'Authorization': 'Bearer ' + key,
                                                        'Content-Type': 'application/json'})
            d = json.load(urllib.request.urlopen(req, timeout=240))
            ch = (d.get('choices') or [{}])[0]
            return {'text': (ch.get('message') or {}).get('content') or '', 'finish': ch.get('finish_reason'),
                    'served_model': d.get('model'), 'provider': d.get('provider'), 'usage': d.get('usage') or {}}
        except urllib.error.HTTPError as e:
            last = f'http {e.code}'
            if e.code in (408, 429, 500, 502, 503, 504):
                time.sleep(2 ** i)
                continue
            break
        except Exception as e:
            last = type(e).__name__
            time.sleep(2 ** i)
    return {'failure': last}


ANSWER_SYSTEM = (
    "You are a personal assistant with a long-term memory. Today is {date}. Before answering, you "
    "ran `engram recall` on the user's question; its output is below exactly as the tool printed it. "
    "Answer the question from it. Be concise. If the memory does not contain the answer, say you "
    "don't know.")


def answer_one(q, r):
    if r.get('failure'):
        return {'qid': q['qid'], 'failure': r['failure']}
    msgs = [{'role': 'system', 'content': ANSWER_SYSTEM.format(date=q['question_date'])},
            {'role': 'user', 'content': f"Question: {q['question']}\n\n`engram recall` output:\n{r['stdout']}"}]
    res = chat(ANSWER_MODEL, msgs, ANSWER_EFFORT)
    if not res.get('failure') and res.get('finish') != 'stop':
        res['failure'] = f"answer finish={res.get('finish')}"
    res['qid'] = q['qid']
    return res


def official_templates():
    """The five templates of LongMemEval's get_anscheck_prompt, by wording.

    Parsed with ast and never executed: a downloaded file is data here."""
    src = open(OFFICIAL).read()
    fn = next(n for n in ast.parse(src).body if isinstance(n, ast.FunctionDef) and n.name == 'get_anscheck_prompt')
    texts = [n.value for n in ast.walk(fn) if isinstance(n, ast.Constant) and isinstance(n.value, str)
             and n.value.count('{}') == 3]
    out = {}
    for t in texts:
        low = t.lower()
        key = ('abstention' if 'unanswerable' in low else 'temporal-reasoning' if 'off-by-one' in low
               else 'knowledge-update' if 'updated answer' in low else 'single-session-preference' if 'rubric' in low
               else 'default')
        out[key] = t
    assert set(out) == {'abstention', 'temporal-reasoning', 'knowledge-update', 'single-session-preference',
                        'default'}, sorted(out)
    return out


def judge_one(templates, q, a):
    if a.get('failure'):
        return {'qid': q['qid'], 'failure': a['failure']}
    key = 'abstention' if q['abstention'] else (q['type'] if q['type'] in templates else 'default')
    prompt = templates[key].format(q['question'], q['answer'], a['text'])
    res = chat(JUDGE_MODEL, [{'role': 'user', 'content': prompt}], max_tokens=10)
    if res.get('failure'):
        return {'qid': q['qid'], 'failure': 'judge ' + res['failure']}
    verdict = (res.get('text') or '').strip().lower()
    if not (verdict.startswith('yes') or verdict.startswith('no')):
        return {'qid': q['qid'], 'failure': f'judge said {verdict[:20]!r}'}
    return {'qid': q['qid'], 'correct': verdict.startswith('yes'), 'served_model': res.get('served_model')}


def answer(args):
    qs = {q['qid']: q for q in jload(f'{WORK}/questions.json')}
    recalls = jload(f'{WORK}/runs/{args.cond}/recall.json')
    templates = official_templates()
    with cf.ThreadPoolExecutor(args.workers) as ex:
        answers = list(ex.map(lambda r: answer_one(qs[r['qid']], r), recalls))
    jdump(f'{WORK}/runs/{args.cond}/answers.json', answers)
    with cf.ThreadPoolExecutor(args.workers) as ex:
        verdicts = list(ex.map(lambda a: judge_one(templates, qs[a['qid']], a), answers))
    jdump(f'{WORK}/runs/{args.cond}/verdicts.json', verdicts)
    print(args.cond, 'answered', len(answers), 'failures', sum(1 for a in answers if a.get('failure')),
          'served', dict(collections.Counter(a.get('served_model') for a in answers)),
          'judge failures', sum(1 for v in verdicts if v.get('failure') and not any(
              a.get('failure') for a in answers if a['qid'] == v['qid'])))


# ---------------------------------------------------------------------- report
def mcnemar_exact(b, c):
    n = b + c
    if n == 0:
        return 1.0
    return min(1.0, 2 * sum(math.comb(n, i) for i in range(min(b, c) + 1)) / 2 ** n)


def wilson(k, n, z=1.96):
    if n == 0:
        return (0.0, 0.0)
    p = k / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return ((c - h) / d * 100, (c + h) / d * 100)


def report(args):
    qs = {q['qid']: q for q in jload(f'{WORK}/questions.json')}
    conds = args.conds.split(',')
    verdicts = {c: {x['qid']: x for x in jload(f'{WORK}/runs/{c}/verdicts.json')} for c in conds}
    recalls = {c: {x['qid']: x for x in jload(f'{WORK}/runs/{c}/recall.json')} for c in conds}
    answers = {c: {x['qid']: x for x in jload(f'{WORK}/runs/{c}/answers.json')} for c in conds}
    ref = conds[0]
    # A question counts only where every arm produced a verdict and retrieved
    # exactly what the reference arm retrieved: the arms then differ only in
    # what recall printed.
    valid, drift, failed = [], [], []
    for q in qs:
        if any('correct' not in verdicts[c].get(q, {}) for c in conds):
            failed.append(q)
        elif any(recalls[c][q].get('result_ids') != recalls[ref][q].get('result_ids') for c in conds):
            drift.append(q)
        else:
            valid.append(q)
    print(f'questions {len(qs)}: compared {len(valid)}, retrieval differed {len(drift)}, failures {len(failed)}')
    types = sorted({qs[q]['type'] for q in valid})
    print('\n| type | n | ' + ' | '.join(conds) + ' |')
    print('|---|---|' + '---|' * len(conds))
    for t in types + ['ALL']:
        ids = [q for q in valid if t == 'ALL' or qs[q]['type'] == t]
        cells = []
        for c in conds:
            k = sum(verdicts[c][q]['correct'] for q in ids)
            lo, hi = wilson(k, len(ids))
            cells.append(f'{100 * k / max(1, len(ids)):.1f} ({lo:.0f}–{hi:.0f})')
        print(f'| {t} | {len(ids)} | ' + ' | '.join(cells) + ' |')
    pairs = [(conds[i - 1], conds[i]) for i in range(1, len(conds))]
    if len(conds) > 2:
        pairs.append((conds[0], conds[-1]))
    for a, b in pairs:
        print(f'\n{b} vs {a} (gained / lost, exact McNemar p):')
        for t in types + ['ALL']:
            ids = [q for q in valid if t == 'ALL' or qs[q]['type'] == t]
            g = sum(1 for q in ids if verdicts[b][q]['correct'] and not verdicts[a][q]['correct'])
            l = sum(1 for q in ids if verdicts[a][q]['correct'] and not verdicts[b][q]['correct'])
            print(f'  {t}: +{g} / -{l}, p={mcnemar_exact(g, l):.3f}')
    print()
    for c in conds:
        rb = [recalls[c][q]['bytes'] for q in valid]
        pt = [answers[c][q]['usage'].get('prompt_tokens') for q in valid if answers[c][q].get('usage', {}).get('prompt_tokens')]
        fired = sum(1 for q in valid if recalls[c][q].get('updates_shown'))
        print(f'{c}: mean recall bytes {int(sum(rb) / max(1, len(rb)))}, mean answer prompt tokens '
              f'{int(sum(pt) / max(1, len(pt)))} (n={len(pt)}), present-tense lines shown in {fired} recalls')


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest='cmd', required=True)
    sub.add_parser('prepare')
    p = sub.add_parser('build'); p.add_argument('--bin', required=True); p.add_argument('--workers', type=int, default=8)
    p = sub.add_parser('recall'); p.add_argument('--bin', required=True); p.add_argument('--cond', required=True)
    p.add_argument('--top-k', type=int, default=0); p.add_argument('--workers', type=int, default=8)
    p = sub.add_parser('answer'); p.add_argument('--cond', required=True); p.add_argument('--workers', type=int, default=8)
    p = sub.add_parser('report'); p.add_argument('--conds', required=True)
    args = ap.parse_args()
    {'prepare': prepare, 'build': build, 'recall': recall, 'answer': answer, 'report': report}[args.cmd](args)


if __name__ == '__main__':
    main()
