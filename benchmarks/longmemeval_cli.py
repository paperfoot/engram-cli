#!/usr/bin/env python3
"""LongMemEval through the real `engram` binary: measures exactly what an agent sees.

The Rust QA bench builds its own context (`[session i — sid]`) and seeds every
memory without an event time, so it cannot measure a change to what `engram
recall` prints. This harness shells out to `engram` exactly as an agent does:

  prepare   pick the questions, attach distractor sessions from the S split
  build     one isolated store per question: `engram remember` each session in
            date order with --event-time (knowledge-update questions with fact
            extraction and --autonomy full; the rest --no-extract-facts)
  recall    copy each store and run `<binary> recall "<question>" --json`
  answer    one answer model reads the question date, the question and the
            recall stdout verbatim
  judge     LongMemEval's type-specific yes/no judge prompts, a different model
            family from the answerer
  report    accuracy per type and condition, paired discordance against a
            reference condition (exact McNemar), recall bytes, prompt tokens

Every condition is a binary built from one commit, so an A/B is "same stores,
same questions, same answerer and judge, different engram". Raw outputs, served
models and token counts are logged under the work dir (not the repo).

Keys come from the environment only and are never printed: GEMINI_API_KEY
(embeddings), TYPESAFE_API_KEY (judge sites, --autonomy full), OPENROUTER_API_KEY
(extraction inside engram, answerer, judge).
"""
import argparse, collections, concurrent.futures as cf, json, math, os, random, shutil, subprocess, sys, time
import urllib.error, urllib.request

DATA = os.environ.get('LME_DATA', os.path.expanduser('~/Projects/engram-cli/data/longmemeval/raw'))
WORK = os.environ.get('LME_WORK', '/tmp/lme')
OR_URL = 'https://openrouter.ai/api/v1/chat/completions'
ANSWER_MODEL = os.environ.get('LME_ANSWER_MODEL', 'openai/gpt-6.1-terra')
ANSWER_EFFORT = os.environ.get('LME_ANSWER_EFFORT', 'low')
JUDGE_MODEL = os.environ.get('LME_JUDGE_MODEL', 'anthropic/claude-haiku-5.5')
DISTRACTORS = int(os.environ.get('LME_DISTRACTORS', '4'))
SEED = 7


def jload(p):
    return json.load(open(p))


def jdump(p, obj):
    os.makedirs(os.path.dirname(p), exist_ok=True)
    tmp = p + '.tmp'
    json.dump(obj, open(tmp, 'w'))
    os.replace(tmp, p)


def lme_date(s):
    """'2023/05/25 (Thu) 20:21' -> '2023-05-25T20:21:00Z'."""
    day, _, hm = s.partition(') ')
    y, m, d = day.split(' ')[0].split('/')
    return f'{y}-{m}-{d}T{hm.strip() or "00:00"}:00Z'


def session_text(turns):
    return '\n'.join(f"{t['role']}: {t['content']}" for t in turns)


# --------------------------------------------------------------------- prepare
def prepare(args):
    oracle = jload(f'{DATA}/oracle.json')
    rng = random.Random(SEED)
    ku = [q for q in oracle if q['question_type'] == 'knowledge-update']
    tr = [q for q in oracle if q['question_type'] == 'temporal-reasoning']
    rest = [q for q in oracle if q['question_type'] not in ('knowledge-update', 'temporal-reasoning')]
    reg = rng.sample(rest, args.regression)
    chosen = {q['question_id']: q for q in ku + tr + reg}
    # Distractors: non-evidence sessions of the same question in the S split.
    print('loading S split for distractors…', file=sys.stderr)
    s_split = {q['question_id']: q for q in jload(f'{DATA}/s.json') if q['question_id'] in chosen}
    out = []
    for qid, q in chosen.items():
        sessions = [{'sid': sid, 'date': lme_date(d), 'text': session_text(t), 'evidence': True}
                    for sid, d, t in zip(q['haystack_session_ids'], q['haystack_dates'], q['haystack_sessions'])]
        sq = s_split.get(qid)
        if sq and DISTRACTORS:
            ev = set(q['haystack_session_ids'])
            pool = [(sid, d, t) for sid, d, t in zip(sq['haystack_session_ids'], sq['haystack_dates'], sq['haystack_sessions'])
                    if sid not in ev]
            for sid, d, t in rng.sample(pool, min(DISTRACTORS, len(pool))):
                sessions.append({'sid': sid, 'date': lme_date(d), 'text': session_text(t), 'evidence': False})
        sessions.sort(key=lambda s: s['date'])
        out.append({'qid': qid, 'type': q['question_type'], 'question': q['question'], 'answer': str(q['answer']),
                    'question_date': q['question_date'], 'abstention': qid.endswith('_abs'),
                    'extract': q['question_type'] == 'knowledge-update', 'sessions': sessions})
    jdump(f'{WORK}/questions.json', out)
    print(json.dumps(collections.Counter(q['type'] for q in out)), 'questions;',
          sum(len(q['sessions']) for q in out), 'sessions')


# ----------------------------------------------------------------------- build
def run_engram(binary, store, argv, timeout=300):
    env = dict(os.environ, ENGRAM_STORE=store, NO_COLOR='1')
    t = time.time()
    p = subprocess.run([binary, *argv], env=env, capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout, p.stderr, time.time() - t


def build_one(binary, q):
    store = f'{WORK}/stores/{q["qid"]}'
    done = f'{store}/.built'
    if os.path.exists(done):
        return q['qid'], 'cached', []
    shutil.rmtree(store, ignore_errors=True)
    os.makedirs(store)
    log = []
    for s in q['sessions']:
        argv = ['remember', s['text'], '--kb', 'lme', '--event-time', s['date'], '--json']
        argv += ['--autonomy', 'full'] if q['extract'] else ['--no-extract-facts']
        code, out, err, secs = run_engram(binary, store, argv)
        try:
            data = json.loads(out).get('data', {})
        except Exception:
            data = {}
        log.append({'sid': s['sid'], 'code': code, 'secs': round(secs, 2),
                    'fact_extraction': data.get('fact_extraction'), 'facts_added': data.get('facts_added'),
                    'err': err[-300:] if code else ''})
        if code:
            return q['qid'], 'failed', log
    jdump(f'{store}/build-log.json', log)
    open(done, 'w').write('ok')
    return q['qid'], 'built', log


def build(args):
    qs = jload(f'{WORK}/questions.json')
    counts = collections.Counter()
    with cf.ThreadPoolExecutor(args.workers) as ex:
        for qid, status, log in ex.map(lambda q: build_one(args.bin, q), qs):
            counts[status] += 1
            if status == 'failed':
                print('FAILED', qid, log[-1] if log else '', file=sys.stderr)
    print(dict(counts))


# ---------------------------------------------------------------------- recall
def recall_one(binary, cond, q, top_k):
    src = f'{WORK}/stores/{q["qid"]}'
    dst = f'{WORK}/runs/{cond}/stores/{q["qid"]}'
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst)
    argv = ['recall', q['question'], '--json']
    if top_k:
        argv += ['--top-k', str(top_k)]
    code, out, err, secs = run_engram(binary, dst, argv)
    shutil.rmtree(dst, ignore_errors=True)
    return {'qid': q['qid'], 'code': code, 'stdout': out, 'bytes': len(out.encode()), 'secs': round(secs, 2),
            'err': err[-300:] if code else ''}


def recall(args):
    qs = [q for q in jload(f'{WORK}/questions.json') if os.path.exists(f'{WORK}/stores/{q["qid"]}/.built')]
    if args.types:
        qs = [q for q in qs if q['type'] in args.types.split(',')]
    with cf.ThreadPoolExecutor(args.workers) as ex:
        rows = list(ex.map(lambda q: recall_one(args.bin, args.cond, q, args.top_k), qs))
    jdump(f'{WORK}/runs/{args.cond}/recall.json', rows)
    bad = [r for r in rows if r['code']]
    print(args.cond, 'recalls', len(rows), 'failed', len(bad), 'mean bytes',
          int(sum(r['bytes'] for r in rows) / max(1, len(rows))))


# ---------------------------------------------------------------- answer/judge
def chat(model, messages, effort=None, max_tokens=800):
    key = os.environ['OPENROUTER_API_KEY']
    body = {'model': model, 'messages': messages, 'temperature': 0.0, 'max_tokens': max_tokens,
            'usage': {'include': True}}
    if effort:
        body['reasoning'] = {'effort': effort}
    data = json.dumps(body).encode()
    for i in range(6):
        try:
            req = urllib.request.Request(OR_URL, data, {'Authorization': 'Bearer ' + key,
                                                        'Content-Type': 'application/json'})
            d = json.load(urllib.request.urlopen(req, timeout=240))
            ch = (d.get('choices') or [{}])[0]
            return {'text': (ch.get('message') or {}).get('content') or '', 'finish': ch.get('finish_reason'),
                    'served_model': d.get('model'), 'provider': d.get('provider'), 'usage': d.get('usage') or {}}
        except urllib.error.HTTPError as e:
            if e.code in (408, 429, 500, 502, 503, 504):
                time.sleep(2 ** i)
                continue
            return {'error': f'http {e.code}'}
        except Exception as e:
            time.sleep(2 ** i)
            err = type(e).__name__
    return {'error': 'retries'}


ANSWER_SYSTEM = (
    "You are a personal assistant with a long-term memory. Today is {date}. Before answering, you "
    "ran `engram recall` on the user's question; its output is below exactly as the tool printed it. "
    "Answer the question from it. Be concise. If the memory does not contain the answer, say you "
    "don't know.")


def answer_one(cond, q, r):
    if r['code']:
        return {'qid': q['qid'], 'error': 'recall failed'}
    msgs = [{'role': 'system', 'content': ANSWER_SYSTEM.format(date=q['question_date'])},
            {'role': 'user', 'content': f"Question: {q['question']}\n\n`engram recall` output:\n{r['stdout']}"}]
    res = chat(ANSWER_MODEL, msgs, ANSWER_EFFORT)
    res['qid'] = q['qid']
    return res


# LongMemEval's official judge prompts (evaluate_qa.py), by question type.
def judge_prompt(q, response):
    if q['abstention']:
        return ("I will give you an unanswerable question, an explanation, and a response from a model. "
                "Please answer yes if the model correctly identifies the question as unanswerable. The model "
                "could say that the information is incomplete, or some other information is given but the "
                "asked information is not.\n\nQuestion: {}\n\nExplanation: {}\n\nModel Response: {}\n\n"
                "Does the model correctly identify the question as unanswerable? Answer yes or no only."
                ).format(q['question'], q['answer'], response)
    base = ("I will give you a question, a correct answer, and a response from a model. Please answer yes "
            "if the response contains the correct answer. Otherwise, answer no. If the response is "
            "equivalent to the correct answer or contains all the intermediate steps to get the correct "
            "answer, you should also answer yes. If the response only contains a subset of the "
            "information required by the answer, answer no. ")
    if q['type'] == 'temporal-reasoning':
        base += ("In addition, do not penalize off-by-one errors for the number of days. If the question "
                 "asks for the number of days/weeks/months, etc., and the model makes off-by-one errors "
                 "(e.g., predicting 19 days when the answer is 18), the model's response is still correct. ")
    elif q['type'] == 'knowledge-update':
        base += ("If the response contains some previous information along with an updated answer, the "
                 "response should be considered as correct as long as the updated answer is the required "
                 "answer. ")
    elif q['type'] == 'single-session-preference':
        base = ("I will give you a question, a rubric for desired personalized response, and a response "
                "from a model. Please answer yes if the response satisfies the desired response. Otherwise, "
                "answer no. The model does not need to reflect all the points in the rubric. The response is "
                "correct as long as it recalls and utilizes the user's personal information correctly. ")
        return base + "\n\nQuestion: {}\n\nRubric: {}\n\nModel Response: {}\n\nIs the model response correct? Answer yes or no only.".format(
            q['question'], q['answer'], response)
    return base + "\n\nQuestion: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\nIs the model response correct? Answer yes or no only.".format(
        q['question'], q['answer'], response)


def judge_one(q, a):
    if a.get('error'):
        return {'qid': q['qid'], 'correct': False, 'error': a['error']}
    res = chat(JUDGE_MODEL, [{'role': 'user', 'content': judge_prompt(q, a['text'])}], max_tokens=10)
    verdict = (res.get('text') or '').strip().lower()
    return {'qid': q['qid'], 'correct': verdict.startswith('yes'), 'verdict': verdict[:20],
            'served_model': res.get('served_model'), 'error': res.get('error')}


def answer(args):
    qs = {q['qid']: q for q in jload(f'{WORK}/questions.json')}
    recalls = jload(f'{WORK}/runs/{args.cond}/recall.json')
    with cf.ThreadPoolExecutor(args.workers) as ex:
        answers = list(ex.map(lambda r: answer_one(args.cond, qs[r['qid']], r), recalls))
    jdump(f'{WORK}/runs/{args.cond}/answers.json', answers)
    with cf.ThreadPoolExecutor(args.workers) as ex:
        verdicts = list(ex.map(lambda a: judge_one(qs[a['qid']], a), answers))
    jdump(f'{WORK}/runs/{args.cond}/verdicts.json', verdicts)
    served = collections.Counter(a.get('served_model') for a in answers)
    print(args.cond, 'answered', len(answers), 'errors', sum(1 for a in answers if a.get('error')),
          'served', dict(served), 'judge errors', sum(1 for v in verdicts if v.get('error')))


# ---------------------------------------------------------------------- report
def mcnemar_exact(b, c):
    n = b + c
    if n == 0:
        return 1.0
    k = min(b, c)
    p = sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n
    return min(1.0, 2 * p)


def report(args):
    qs = {q['qid']: q for q in jload(f'{WORK}/questions.json')}
    conds = args.conds.split(',')
    v = {c: {x['qid']: x['correct'] for x in jload(f'{WORK}/runs/{c}/verdicts.json')} for c in conds}
    rec = {c: {x['qid']: x for x in jload(f'{WORK}/runs/{c}/recall.json')} for c in conds}
    ans = {c: {x['qid']: x for x in jload(f'{WORK}/runs/{c}/answers.json')} for c in conds}
    types = sorted({qs[q]['type'] for c in conds for q in v[c]})
    print('| type | n | ' + ' | '.join(conds) + ' |')
    print('|---|---|' + '---|' * len(conds))
    for t in types + ['ALL']:
        common = set.intersection(*[set(v[c]) for c in conds])
        ids = [q for q in common if t == 'ALL' or qs[q]['type'] == t]
        cells = [f"{100 * sum(v[c][q] for q in ids) / max(1, len(ids)):.1f}" for c in conds]
        print(f'| {t} | {len(ids)} | ' + ' | '.join(cells) + ' |')
    ref = conds[0]
    for c in conds[1:]:
        common = set(v[ref]) & set(v[c])
        b = sum(1 for q in common if v[c][q] and not v[ref][q])
        cc = sum(1 for q in common if v[ref][q] and not v[c][q])
        print(f'{c} vs {ref}: gained {b}, lost {cc}, exact McNemar p={mcnemar_exact(b, cc):.3f}')
    for c in conds:
        rb = [x['bytes'] for x in rec[c].values()]
        pt = [x.get('usage', {}).get('prompt_tokens') or 0 for x in ans[c].values()]
        print(f'{c}: mean recall bytes {int(sum(rb) / max(1, len(rb)))}, mean answer prompt tokens '
              f'{int(sum(pt) / max(1, len(pt)))}')


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest='cmd', required=True)
    p = sub.add_parser('prepare'); p.add_argument('--regression', type=int, default=40)
    p = sub.add_parser('build'); p.add_argument('--bin', required=True); p.add_argument('--workers', type=int, default=8)
    p = sub.add_parser('recall'); p.add_argument('--bin', required=True); p.add_argument('--cond', required=True)
    p.add_argument('--types', default=''); p.add_argument('--top-k', type=int, default=0)
    p.add_argument('--workers', type=int, default=8)
    p = sub.add_parser('answer'); p.add_argument('--cond', required=True); p.add_argument('--workers', type=int, default=8)
    p = sub.add_parser('report'); p.add_argument('--conds', required=True)
    args = ap.parse_args()
    {'prepare': prepare, 'build': build, 'recall': recall, 'answer': answer, 'report': report}[args.cmd](args)


if __name__ == '__main__':
    main()
