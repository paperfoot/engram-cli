#!/usr/bin/env python3
"""Pick remember's fact-extraction model by measurement (October 2026 model refresh).

Memories: the September remember set (60 seeded LoCoMo windows + the 20 health notes in
benchmarks/jev/remember-health-notes.jsonl), as written by build_remember_set.py to
<data>/pairs/remember-memories.jsonl.

  extract MODEL..  - remember's extraction call, verbatim (facts.rs FACT_EXTRACTION_PROMPT +
                     the DATE CONTEXT system message, temperature 0, max_tokens 2048, the same
                     request engram's OpenRouter client sends). Records cost (OpenRouter
                     usage.cost) and wall latency per call.
  judge MODEL..    - grounding: every extracted fact through the SHIPPED grounding question
                     (`judge_eval grounding`, jev pinned in typesafe.rs). A fact with p < 0.9
                     counts as unsupported.
                     safety recall: for each health note, the gold safety facts below are
                     checked against that note's extracted fact list with one JEV noul each.
  label MODEL..    - second opinion, independent of JEV: an LLM checker (LABEL_MODEL) labels every
                     fact supported / not_supported with the September remember-set rubric
                     (jev/build_remember_set.py LABEL_SYSTEM).
  report MODEL..   - one table row per model.

Keys from the environment only (OPENROUTER_API_KEY, TYPESAFE_API_KEY), never printed.
Outputs go to <data>/models-2026-10/ (gitignored data dir); only the table is committed.
"""
import collections, concurrent.futures as cf, json, os, subprocess, sys, time, urllib.error, urllib.request

DATA = os.environ.get('ENGRAM_EVAL_DATA', os.path.expanduser('~/Projects/engram-jev/data'))
OUT = f'{DATA}/models-2026-10'
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
JUDGE_EVAL = os.environ.get('JUDGE_EVAL', f'{REPO}/target/debug/examples/judge_eval')
OR_URL = 'https://openrouter.ai/api/v1/chat/completions'
TS_URL = 'https://api.typesafe.ai/v1/systemone'
JEV_MODEL = os.environ.get('JEV_MODEL', 'jev-1.13.0')
UNSUPPORTED_BELOW = 0.9
LABEL_MODEL = os.environ.get('LABEL_MODEL', 'anthropic/claude-sonnet-5.5')

# What a health memory must not lose: medications (current and stopped), allergies and
# non-allergies, diagnoses ruled in or out. Hedged notes (hn-05, hn-18) carry no gold fact.
SAFETY_GOLD = {
    'hn-01': ['Sam takes metformin 500 mg twice a day'],
    'hn-02': ['Ana no longer takes omeprazole', 'Ana takes levothyroxine 50 mcg'],
    'hn-03': ['Leo is not taking a statin'],
    'hn-04': ['Ana is not allergic to penicillin', 'Ana is allergic to cats', 'Ana is allergic to dust mites'],
    'hn-06': ['Sam takes lisinopril 20 mg'],
    'hn-07': ['Leo has mild asthma or a history of it'],
    'hn-08': ['Ana tested negative for celiac disease', "Ana's sister has celiac disease"],
    'hn-09': ['Bob no longer takes amoxicillin'],
    'hn-10': ['Sam should avoid ibuprofen'],
    'hn-11': ['Leo takes sertraline 50 mg'],
    'hn-12': ['Ana has a thyroid nodule'],
    'hn-13': ['Bob no longer takes atorvastatin', 'Bob takes rosuvastatin 10 mg'],
    'hn-14': ["Sam's mother takes warfarin", 'Sam takes no blood thinners'],
    'hn-15': ['Leo does not have appendicitis', 'Leo had gastroenteritis'],
    'hn-16': ['Ana is vegetarian', 'Ana takes a B12 supplement'],
    'hn-17': ['Bob has a severe peanut allergy', 'Bob carries EpiPens'],
    'hn-19': ["Leo's blood type is B positive"],
    'hn-20': ['Ana no longer takes metformin', 'Ana had a gastric sleeve'],
}


def split_effort(model):
    """`openai/gpt-5.6-luna@minimal` -> the slug plus an OpenRouter reasoning effort."""
    return model.split('@', 1) if '@' in model else (model, None)


def slug(model):
    return model.replace('/', '__')


def load_jsonl(path):
    return [json.loads(l) for l in open(path) if l.strip()]


def write_jsonl(path, rows):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w') as f:
        for r in rows:
            f.write(json.dumps(r) + '\n')


def fact_prompt_text():
    src = open(f'{REPO}/crates/engram-graph/src/facts.rs').read()
    head = 'const FACT_EXTRACTION_PROMPT: &str = r#"'
    start = src.index(head) + len(head)
    return src[start:src.index('"#;', start)]


def system_prompt(date):
    # facts.rs extraction_system_prompt, verbatim.
    return (fact_prompt_text() + "\n\nDATE CONTEXT:\nThis memory is dated " + date + ". Resolve every relative expression "
            "(\"last March\", \"two years ago\", \"since the summer\") against that date and emit valid_from as an absolute "
            "YYYY-MM-DD. If you cannot resolve one to an absolute date, emit null — an omitted date is inherited from the "
            "memory, a guessed one silently overwrites true history.")


def first_json_array(text):
    text = text.strip()
    if text.startswith('```'):
        text = text.split('\n', 1)[1].rsplit('```', 1)[0]
    i = text.index('[')
    depth, in_str, esc = 0, False, False
    for j in range(i, len(text)):
        c = text[j]
        if in_str:
            esc = (c == '\\' and not esc)
            if c == '"' and not esc:
                in_str = False
            continue
        if c == '"':
            in_str = True
        elif c == '[':
            depth += 1
        elif c == ']':
            depth -= 1
            if depth == 0:
                return json.loads(text[i:j + 1])
    raise ValueError('unterminated array')


def chat(model, messages, retries=5, max_tokens=2048):
    key = os.environ['OPENROUTER_API_KEY']
    model, effort = split_effort(model)
    req_body = {'model': model, 'messages': messages, 'temperature': 0.0, 'max_tokens': max_tokens, 'usage': {'include': True}}
    if effort:
        req_body['reasoning'] = {'effort': effort}
    body = json.dumps(req_body).encode()
    err = None
    for i in range(retries):
        t = time.time()
        try:
            req = urllib.request.Request(OR_URL, body, {'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
            d = json.load(urllib.request.urlopen(req, timeout=180))
            return (d['choices'][0]['message'].get('content') or ''), d.get('usage') or {}, time.time() - t
        except urllib.error.HTTPError as e:
            err = f'{e.code}'
            if e.code in (408, 429, 500, 502, 503, 504):
                time.sleep(2 ** i)
                continue
            raise RuntimeError(err)
        except Exception as e:
            err = type(e).__name__
            time.sleep(2 ** i)
    raise RuntimeError(err)


def extract(model):
    mems = load_jsonl(f'{DATA}/pairs/remember-memories.jsonl')

    def job(m):
        try:
            text, usage, lat = chat(model, [{'role': 'system', 'content': system_prompt(m['date'])},
                                            {'role': 'user', 'content': m['text']}])
        except RuntimeError as e:
            return m, [], {}, None, f'request:{e}'
        try:
            facts = first_json_array(text)
        except Exception:
            return m, [], usage, lat, 'parse'
        facts = [f for f in facts if isinstance(f, dict) and all(str(f.get(x) or '').strip() for x in ('subject', 'predicate', 'object'))]
        return m, facts, usage, lat, None

    items, calls = [], []
    with cf.ThreadPoolExecutor(8) as ex:
        for m, facts, usage, lat, err in ex.map(job, mems):
            calls.append({'mem_id': m['mem_id'], 'latency_s': lat, 'cost': usage.get('cost'),
                          'prompt_tokens': usage.get('prompt_tokens'), 'completion_tokens': usage.get('completion_tokens'),
                          'reasoning_tokens': (usage.get('completion_tokens_details') or {}).get('reasoning_tokens'),
                          'error': err, 'facts': len(facts)})
            for k, f in enumerate(facts):
                items.append({'id': f"{m['mem_id']}-f{k}", 'doc_id': m['mem_id'], 'source': m['source'],
                              'subject': str(f['subject']).strip(), 'predicate': str(f['predicate']).strip(),
                              'object': str(f['object']).strip(), 'state': f.get('state') or 'ongoing'})
    d = f'{OUT}/{slug(model)}'
    write_jsonl(f'{d}/items.jsonl', items)
    write_jsonl(f'{d}/calls.jsonl', calls)
    print(model, 'facts', len(items), 'errors', collections.Counter(c['error'] for c in calls if c['error']))


def claim(it):
    pred = it['predicate'].replace('_', ' ')
    if it['state'] == 'ended':
        return f"{it['subject']} no longer {pred} {it['object']}"
    return f"{it['subject']} {pred} {it['object']}"


def ask_jev(state, questions, retries=4):
    key = os.environ['TYPESAFE_API_KEY']
    body = json.dumps({'model': JEV_MODEL, 'state': state, 'questions': questions}).encode()
    for i in range(retries):
        try:
            req = urllib.request.Request(TS_URL, body, {'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
            return json.load(urllib.request.urlopen(req, timeout=30))
        except Exception:
            time.sleep(2 ** i)
    return {}


def judge(model):
    d = f'{OUT}/{slug(model)}'
    mems = load_jsonl(f'{DATA}/pairs/remember-memories.jsonl')
    items = load_jsonl(f'{d}/items.jsonl')
    write_jsonl(f'{d}/docs.jsonl', [{'doc_id': m['mem_id'], 'title': '', 'text': m['text']} for m in mems])
    write_jsonl(f'{d}/pairs.jsonl', [{'id': it['id'], 'doc_id': it['doc_id'], 'claim': claim(it), 'label': 'NEI'} for it in items])
    subprocess.run([JUDGE_EVAL, 'grounding', '--pairs', f'{d}/pairs.jsonl', '--docs', f'{d}/docs.jsonl',
                    '--out', f'{d}/grounding.jsonl', '--route', 'typesafe'], check=True)

    by_note = collections.defaultdict(list)
    for it in items:
        by_note[it['doc_id']].append(it)

    def job(note):
        listing = '\n'.join(f'- {claim(it)}' for it in by_note.get(note, [])) or '(no facts)'
        qs = {f'g{k}': {'type': 'noul',
                        'instructions': {'fact': g, 'question': 'Does `facts` state `fact`, or something that directly entails it?'},
                        'criteria': {'true': 'One of the listed facts states it or a more specific version of it.',
                                     'false': 'No listed fact states it, or the list says the opposite, or only a hedged version.'}}
              for k, g in enumerate(SAFETY_GOLD[note])}
        r = ask_jev({'facts': listing}, qs)
        return note, {g: (r.get('answers', {}).get(f'g{k}') or {}).get('noul') for k, g in enumerate(SAFETY_GOLD[note])}

    with cf.ThreadPoolExecutor(8) as ex:
        safety = dict(ex.map(job, list(SAFETY_GOLD)))
    json.dump(safety, open(f'{d}/safety.json', 'w'), indent=1)


def label(model):
    sys.path.insert(0, os.path.join(REPO, 'benchmarks', 'jev'))
    from build_remember_set import LABEL_SYSTEM
    d = f'{OUT}/{slug(model)}'
    mems = {m['mem_id']: m for m in load_jsonl(f'{DATA}/pairs/remember-memories.jsonl')}
    by = collections.defaultdict(list)
    for it in load_jsonl(f'{d}/items.jsonl'):
        by[it['doc_id']].append(it)

    def render(it):
        return ('[ENDED] ' if it['state'] == 'ended' else '') + f"{it['subject']} {it['predicate']} {it['object']}"

    def job(mid):
        its = by[mid]
        listing = '\n'.join(f'{n + 1}. {render(it)}' for n, it in enumerate(its))
        got, cost = {}, 0.0
        for _ in range(2):
            try:
                text, usage, _lat = chat(LABEL_MODEL, [{'role': 'system', 'content': LABEL_SYSTEM},
                                                       {'role': 'user', 'content': f"MEMORY TEXT:\n{mems[mid]['text']}\n\nTRIPLES:\n{listing}"}],
                                         max_tokens=8192)
                cost += usage.get('cost') or 0
                got = {int(x['n']): x for x in json.loads(text[text.index('{'):text.rindex('}') + 1])['items']}
                break
            except Exception:
                continue
        return [{'id': it['id'], 'label': (got.get(n + 1) or {}).get('label'), 'class': (got.get(n + 1) or {}).get('class')}
                for n, it in enumerate(its)], cost

    rows, cost = [], 0.0
    with cf.ThreadPoolExecutor(8) as ex:
        for out, c in ex.map(job, list(by)):
            rows.extend(out)
            cost += c
    write_jsonl(f'{d}/labels.jsonl', rows)
    print(model, 'labelled', len(rows), collections.Counter(r['label'] for r in rows), 'usd %.3f' % cost)


def pct(xs, q):
    xs = sorted(x for x in xs if x is not None)
    return xs[min(len(xs) - 1, int(len(xs) * q))] if xs else None


def report(models):
    rows = []
    for model in models:
        d = f'{OUT}/{slug(model)}'
        calls = load_jsonl(f'{d}/calls.jsonl')
        g = [r for r in load_jsonl(f'{d}/grounding.jsonl') if 'summary' not in r]
        ps = [r['p'] for r in g]
        unanswered = sum(p is None for p in ps)
        unsup = sum(1 for p in ps if p is not None and p < UNSUPPORTED_BELOW)
        health_ids = {r['id'] for r in load_jsonl(f'{d}/items.jsonl') if r['source'] == 'health_note'}
        hp = [r['p'] for r in g if r['id'] in health_ids and r['p'] is not None]
        safety = json.load(open(f'{d}/safety.json'))
        gold = [p for note in safety.values() for p in note.values()]
        recalled = sum(1 for p in gold if p is not None and p >= 0.5)
        labels = [r['label'] for r in load_jsonl(f'{d}/labels.jsonl')] if os.path.exists(f'{d}/labels.jsonl') else []
        labelled = [x for x in labels if x in ('supported', 'not_supported')]
        llm_unsup = (f"{round(100 * labelled.count('not_supported') / len(labelled), 1)} ({labelled.count('not_supported')})"
                     if labelled else '-')
        cost = sum(c['cost'] or 0 for c in calls)
        n_ok = sum(1 for c in calls if not c['error'])
        rows.append({'model': model, 'memories': len(calls), 'failed_calls': len(calls) - n_ok, 'facts': len(ps),
                     'unsupported_pct': round(100 * unsup / max(len(ps) - unanswered, 1), 1),
                     'unsupported_health_pct': round(100 * sum(p < UNSUPPORTED_BELOW for p in hp) / max(len(hp), 1), 1),
                     'unsupported_n': unsup, 'unanswered': unanswered, 'llm_unsupported': llm_unsup,
                     'safety_recall': f'{recalled}/{len(gold)}',
                     'usd_per_1k': round(1000 * cost / max(len(calls), 1), 3),
                     'p50_s': round(pct([c['latency_s'] for c in calls], .5), 2),
                     'p95_s': round(pct([c['latency_s'] for c in calls], .95), 2)})
    print(f'| model | facts | JEV unsupported % (n), p<0.9 | JEV unsupported % health | {LABEL_MODEL} not_supported % (n) '
          '| safety recall | $/1k memories | p50 s | p95 s | failed calls |')
    print('|---|---|---|---|---|---|---|---|---|---|')
    for r in rows:
        print(f"| `{r['model']}` | {r['facts']} | {r['unsupported_pct']} ({r['unsupported_n']}) | {r['unsupported_health_pct']} | "
              f"{r['llm_unsupported']} | {r['safety_recall']} | "
              f"{r['usd_per_1k']} | {r['p50_s']} | {r['p95_s']} | {r['failed_calls']} |")
    json.dump(rows, open(f'{OUT}/report.json', 'w'), indent=1)


if __name__ == '__main__':
    cmd, models = sys.argv[1], sys.argv[2:]
    if cmd == 'report':
        report(models)
    else:
        fn = {'extract': extract, 'judge': judge, 'label': label}[cmd]
        with cf.ThreadPoolExecutor(len(models)) as ex:
            list(ex.map(fn, models))
