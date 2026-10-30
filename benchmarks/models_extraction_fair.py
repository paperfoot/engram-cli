#!/usr/bin/env python3
"""Extraction bake-off under remember's production request (same prompt, max_tokens 2048, reasoning
effort low for every model), scored on what engram consumes: safety facts filed under engram's
own canonical relations with the right state, false safety records, unusable outputs. Records
the served model, provider and raw output of every call.

A model id without a slash (`claude-haiku-5-5@low`) goes to the Anthropic Messages API directly,
as engram's AnthropicClient sends it: system prompt as `system`, no temperature, effort in
`output_config`, max_tokens 8192 because thinking counts against the cap. Cost is computed from
the token counts at the published price.

usage: models_extraction_fair.py extract MODEL.. | score MODEL.. | ground MODEL..
  ground: the first pass's JEV grounding and safety recall (models_extraction_eval.py judge),
          then its report row, so a direct model sits in the same table as the first pass.
"""
import collections, concurrent.futures as cf, importlib.util, json, os, re, sys, time, urllib.error, urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location('mx', os.path.join(REPO, 'benchmarks', 'models_extraction_eval.py'))
mx = importlib.util.module_from_spec(spec); spec.loader.exec_module(mx)
OUT = f'{mx.DATA}/models-fair'

# engram-core relations.rs canonical_relation, parsed from source so the scorer uses engram's own map.
def load_alias_map():
    src = open(f'{REPO}/crates/engram-core/src/relations.rs').read()
    body = src[src.index('pub fn canonical_relation'):src.index('pub fn cessation_of')]
    amap = {}
    for arm in re.finditer(r'((?:"[a-z_]+"\s*\|?\s*)+)=>\s*"([a-z_]+)"', body):
        for a in re.findall(r'"([a-z_]+)"', arm.group(1)):
            amap[a] = arm.group(2)
    return amap
ALIAS = load_alias_map()
def canon(p):
    p = p.strip().lower().replace(' ', '_').replace('-', '_')
    return ALIAS.get(p, p)

# Structured key for the 20 health notes. pos: a fact with this canonical relation must exist
# (subject contains `subj`, object contains `obj`, state as given or 'any').
# neg: no ONGOING fact with this relation/subject/object may exist — that would be a false
# safety record (a non-allergy stored as an allergy, a stopped drug still active, a ruled-out
# diagnosis stored as current, a hedge stored as fact).
POS = [
 ('hn-01','sam','takes_medication','metformin','ongoing'),
 ('hn-02','ana','takes_medication','omeprazole','ended'),
 ('hn-02','ana','takes_medication','levothyroxine','ongoing'),
 ('hn-04','ana','allergic_to','cat','ongoing'),
 ('hn-04','ana','allergic_to','dust','ongoing'),
 ('hn-06','sam','takes_medication','lisinopril','ongoing'),
 ('hn-07','leo','diagnosed_with','asthma','any'),
 ('hn-08','sister','diagnosed_with','celiac','ongoing'),
 ('hn-09','bob','takes_medication','amoxicillin','ended'),
 ('hn-11','leo','takes_medication','sertraline','ongoing'),
 ('hn-12','ana','diagnosed_with','nodule','ongoing'),
 ('hn-13','bob','takes_medication','atorvastatin','ended'),
 ('hn-13','bob','takes_medication','rosuvastatin','ongoing'),
 ('hn-14','mother','takes_medication','warfarin','ongoing'),
 ('hn-15','leo','diagnosed_with','gastroenteritis','any'),
 ('hn-17','bob','allergic_to','peanut','ongoing'),
 ('hn-20','ana','takes_medication','metformin','ended'),
]
NEG = [  # (note, subject must equal/contain, exclude-subject-substring, relation, object)
 ('hn-02','ana',None,'takes_medication','omeprazole'),
 ('hn-03','leo',None,'takes_medication','statin'),
 ('hn-04','ana',None,'allergic_to','penicillin'),
 ('hn-05','bob',None,'diagnosed_with','lactose'),
 ('hn-08','ana','sister','diagnosed_with','celiac'),
 ('hn-09','bob',None,'takes_medication','amoxicillin'),
 ('hn-12','ana',None,'diagnosed_with','cancer'),
 ('hn-13','bob',None,'takes_medication','atorvastatin'),
 ('hn-14','sam','mother','takes_medication','warfarin'),
 ('hn-15','leo',None,'diagnosed_with','appendicitis'),
 ('hn-17','son',None,'allergic_to',''),
 ('hn-18','sam',None,'takes_medication','melatonin'),
 ('hn-20','ana',None,'takes_medication','metformin'),
]

ANTHROPIC_URL = 'https://api.anthropic.com/v1/messages'
ANTHROPIC_PRICE = {'claude-haiku-5-5': (0.10, 0.50)}  # USD per M tokens in/out, 30 Oct 2026

def chat_anthropic(model, messages):
    """One call shaped like engram's AnthropicClient, returned in OpenRouter's response shape."""
    key = os.environ['ANTHROPIC_API_KEY']
    model, effort = mx.split_effort(model)
    req = {'model': model, 'max_tokens': 8192,
           'system': '\n\n'.join(m['content'] for m in messages if m['role'] == 'system'),
           'messages': [m for m in messages if m['role'] != 'system']}
    if effort:
        req['output_config'] = {'effort': effort}
    body = json.dumps(req).encode()
    headers = {'x-api-key': key, 'anthropic-version': '2023-06-01', 'content-type': 'application/json'}
    for i in range(5):
        t = time.time()
        try:
            d = json.load(urllib.request.urlopen(urllib.request.Request(ANTHROPIC_URL, body, headers), timeout=180))
        except urllib.error.HTTPError as e:
            if e.code in (408, 429, 500, 502, 503, 504, 529): time.sleep(2 ** i); continue
            return {'http_error': e.code}, None
        except Exception:
            time.sleep(2 ** i); continue
        lat = time.time() - t
        u = d.get('usage') or {}
        p_in, p_out = ANTHROPIC_PRICE.get(model, (None, None))
        cost = (u.get('input_tokens', 0) * p_in + u.get('output_tokens', 0) * p_out) / 1e6 if p_in is not None else None
        text = ''.join(b.get('text') or '' for b in d.get('content') or [] if b.get('type') == 'text')
        finish = {'max_tokens': 'length', 'end_turn': 'stop'}.get(d.get('stop_reason'), d.get('stop_reason'))
        return {'model': d.get('model'), 'provider': 'Anthropic',
                'choices': [{'message': {'content': text}, 'finish_reason': finish}],
                'usage': {'cost': cost, 'completion_tokens': u.get('output_tokens')}}, lat
    return {'http_error': 'retries'}, None

def chat(model, messages):
    if '/' not in model:
        return chat_anthropic(model, messages)
    key = os.environ['OPENROUTER_API_KEY']
    body = json.dumps({'model': model, 'messages': messages, 'temperature': 0.0, 'max_tokens': 2048,
                       'reasoning': {'effort': 'low'}, 'usage': {'include': True}}).encode()
    for i in range(5):
        t = time.time()
        try:
            req = urllib.request.Request(mx.OR_URL, body, {'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
            return json.load(urllib.request.urlopen(req, timeout=180)), time.time() - t
        except urllib.error.HTTPError as e:
            if e.code in (408, 429, 500, 502, 503, 504): time.sleep(2 ** i); continue
            return {'http_error': e.code}, None
        except Exception as e:
            time.sleep(2 ** i)
    return {'http_error': 'retries'}, None

def extract(model):
    mems = mx.load_jsonl(f'{mx.DATA}/pairs/remember-memories.jsonl')
    def job(m):
        d, lat = chat(model, [{'role': 'system', 'content': mx.system_prompt(m['date'])}, {'role': 'user', 'content': m['text']}])
        ch = (d.get('choices') or [{}])[0]
        raw = (ch.get('message') or {}).get('content') or ''
        u = d.get('usage') or {}
        rec = {'mem_id': m['mem_id'], 'served_model': d.get('model'), 'provider': d.get('provider'),
               'finish_reason': ch.get('finish_reason'), 'latency_s': lat, 'cost': u.get('cost'),
               'completion_tokens': u.get('completion_tokens'),
               'reasoning_tokens': (u.get('completion_tokens_details') or {}).get('reasoning_tokens'),
               'http_error': d.get('http_error'), 'raw': raw, 'facts': []}
        try:
            fs = mx.first_json_array(raw)
            rec['facts'] = [f for f in fs if isinstance(f, dict) and all(str(f.get(x) or '').strip() for x in ('subject', 'predicate', 'object'))]
        except Exception:
            rec['parse_error'] = True
        return rec
    with cf.ThreadPoolExecutor(8) as ex:
        rows = list(ex.map(job, mems))
    os.makedirs(OUT, exist_ok=True)
    mx.write_jsonl(f'{OUT}/{mx.slug(model)}.jsonl', rows)
    print(model, 'calls', len(rows), 'parse_errors', sum(1 for r in rows if r.get('parse_error')),
          'served', collections.Counter(r['served_model'] for r in rows), 'providers', collections.Counter(r['provider'] for r in rows),
          'finish', collections.Counter(r['finish_reason'] for r in rows))

def score(model):
    rows = {r['mem_id']: r for r in mx.load_jsonl(f'{OUT}/{mx.slug(model)}.jsonl')}
    def facts(note):
        return [(f['subject'].lower(), canon(f['predicate']), f['object'].lower(), (f.get('state') or 'ongoing').lower(),
                 bool(f.get('valid_until'))) for f in rows[note]['facts']]
    pos_ok, pos_miss = 0, []
    for note, s, rel, o, st in POS:
        hit = [f for f in facts(note) if s in f[0] and f[1] == rel and o in f[2]]
        good = [f for f in hit if st == 'any' or (st == 'ended') == (f[3] == 'ended' or f[4])]
        if good: pos_ok += 1
        else: pos_miss.append(f'{note}:{s}/{rel}/{o}/{st}' + (' (wrong state)' if hit else ''))
    bad = []
    for note, s, excl, rel, o in NEG:
        for f in facts(note):
            if s in f[0] and not (excl and excl in f[0]) and f[1] == rel and o in f[2] and f[3] != 'ended' and not f[4]:
                bad.append(f'{note}:{f[0]}/{f[1]}/{f[2]}')
    noncanon = sum(1 for n in rows if n.startswith('hn-') for f in rows[n]['facts']
                   if re.search(r'allerg|medic|drug|prescri|diagnos|takes|taking', f['predicate'].lower())
                   and canon(f['predicate']) not in ('allergic_to', 'takes_medication', 'diagnosed_with'))
    lat = sorted(r['latency_s'] for r in rows.values() if r['latency_s'])
    cost = sum(r['cost'] or 0 for r in rows.values())
    print(f"\n== {model}\n safety facts captured correctly: {pos_ok}/{len(POS)}\n false safety records: {len(bad)}  {bad}\n"
          f" missed: {pos_miss}\n safety-looking predicates engram does not recognise: {noncanon}\n"
          f" unusable outputs: {sum(1 for r in rows.values() if r.get('parse_error') or r.get('http_error'))}/80"
          f"  truncated: {sum(1 for r in rows.values() if r['finish_reason'] == 'length')}\n"
          f" p50 {lat[len(lat)//2]:.2f}s p95 {lat[int(len(lat)*.95)]:.2f}s  $/1k memories {cost/len(rows)*1000:.3f}")

def ground(model):
    rows = mx.load_jsonl(f'{OUT}/{mx.slug(model)}.jsonl')
    src = {m['mem_id']: m['source'] for m in mx.load_jsonl(f'{mx.DATA}/pairs/remember-memories.jsonl')}
    d = f'{mx.OUT}/{mx.slug(model)}'
    mx.write_jsonl(f'{d}/items.jsonl', [
        {'id': f"{r['mem_id']}-f{k}", 'doc_id': r['mem_id'], 'source': src[r['mem_id']],
         'subject': str(f['subject']).strip(), 'predicate': str(f['predicate']).strip(),
         'object': str(f['object']).strip(), 'state': f.get('state') or 'ongoing'}
        for r in rows for k, f in enumerate(r['facts'])])
    mx.write_jsonl(f'{d}/calls.jsonl', [
        {'mem_id': r['mem_id'], 'latency_s': r['latency_s'], 'cost': r['cost'],
         'error': 'parse' if r.get('parse_error') else (f"request:{r['http_error']}" if r.get('http_error') else None),
         'facts': len(r['facts'])} for r in rows])
    mx.judge(model)

if __name__ == '__main__':
    cmd, models = sys.argv[1], sys.argv[2:]
    for m in models:
        {'extract': extract, 'score': score, 'ground': ground}[cmd](m)
    if cmd == 'ground':
        mx.report(models)
