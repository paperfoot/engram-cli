#!/usr/bin/env python3
"""Pair eval set for the fact write path's judgement sites (dedupe + adjudication).

No existing bench exercises the novelty gate or the adjudicator (MemoryAgentBench
conflict resolution is retrieval + answering over an in-memory store), so this
builds labelled (new fact, existing fact) pairs and scores judges on them directly.

  mine      - single-valued update pairs mined from MemoryAgentBench FactConsolidation
              contexts: numbered templated facts where a later fact overwrites an earlier
              one about the same subject and relation. Gold: same_slot, contradicts,
              supersede. Written to data/pairs/ (gitignored; upstream dataset).
  merge     - handwritten fixture (benchmarks/jev/pairs-handwritten.jsonl: safety sets,
              ends, dose changes, restatements, paraphrase duplicates, look-alikes,
              corrections) + mined pairs -> data/pairs/pairs.jsonl.
  baseline  - today's `--autonomy full` adjudicator, verbatim: facts.rs
              ADJUDICATION_PROMPT as the system message, the fact_write.rs rendering
              (new fact as "subject relation object", no memory text; existing as
              "id | subject predicate object"), the default fact model
              google/gemini-3.5-flash-lite, temperature 0, one candidate per call so the
              per-pair confusion matrix is unambiguous. Unparseable replies are recorded
              as such (production silently maps them to coexist).

OPENROUTER_API_KEY is read from the environment and never printed.
"""
import collections, concurrent.futures as cf, json, os, random, re, sys, time, urllib.error, urllib.request

MAB = os.environ.get('MAB_CR', '/Users/dev/Projects/engram-cli/data/memoryagentbench/Conflict_Resolution.jsonl')
OUT = 'data/pairs'
OR_URL = 'https://openrouter.ai/api/v1/chat/completions'
FACT_MODEL = 'google/gemini-3.5-flash-lite'

TEMPLATES = [
    (re.compile(r'^The (?P<p>.+?) of (?P<s>.+?) is (?P<o>.+)\.$'), None),
    (re.compile(r'^(?P<s>.+?) was born in the city of (?P<o>.+)\.$'), 'born_in_city'),
    (re.compile(r'^(?P<s>.+?) died in the city of (?P<o>.+)\.$'), 'died_in_city'),
    (re.compile(r'^(?P<s>.+?) plays the position of (?P<o>.+)\.$'), 'plays_position'),
    (re.compile(r'^(?P<s>.+?) is married to (?P<o>.+)\.$'), 'married_to'),
    (re.compile(r'^(?P<s>.+?) is located in the continent of (?P<o>.+)\.$'), 'located_in_continent'),
    (re.compile(r'^(?P<s>.+?) worked in the city of (?P<o>.+)\.$'), 'worked_in_city'),
    (re.compile(r'^(?P<s>.+?) is a citizen of (?P<o>.+)\.$'), 'citizen_of'),
    (re.compile(r'^(?P<s>.+?) plays (?P<o>.+)\.$'), 'plays_sport'),
    (re.compile(r'^(?P<s>.+?) speaks the language of (?P<o>.+)\.$'), 'speaks_language'),
    (re.compile(r'^(?P<s>.+?) is affiliated with the religion of (?P<o>.+)\.$'), 'religion'),
    (re.compile(r'^(?P<s>.+?) was created by (?P<o>.+)\.$'), 'created_by'),
    (re.compile(r'^(?P<s>.+?) is employed by (?P<o>.+)\.$'), 'employed_by'),
]


def parse_fact(sentence):
    for rx, pred in TEMPLATES:
        m = rx.match(sentence.strip())
        if m:
            p = pred or m.group('p').replace(' ', '_')
            return {'subject': m.group('s'), 'predicate': p, 'object': m.group('o')}
    return None


def mine(n_pairs=100, seed=20260925):
    pairs = []
    for row in (json.loads(l) for l in open(MAB)):
        facts = re.findall(r'^(\d+)\. (.+)$', row['context'], re.M)
        seen = {}
        for num, sent in facts:
            f = parse_fact(sent)
            if not f:
                continue
            key = (f['subject'], f['predicate'])
            if key in seen and seen[key][1]['object'] != f['object']:
                old_sent, old = seen[key]
                pairs.append({'id': f"mab-{row['metadata']['qa_pair_ids'][0].rsplit('_no', 1)[0]}-{num}", 'group': 'mab_single_update',
                              'new_text': sent, 'new_fact': f, 'existing': old,
                              'gold': {'restates': False, 'same_slot': True, 'contradicts': True, 'old_ended': False, 'action': 'supersede'}})
            seen[key] = (sent, f)
    random.Random(seed).shuffle(pairs)
    uniq, keys = [], set()
    for p in pairs:
        k = (p['new_fact']['subject'], p['new_fact']['predicate'], p['new_fact']['object'], p['existing']['object'])
        if k not in keys:
            keys.add(k)
            uniq.append(p)
    os.makedirs(OUT, exist_ok=True)
    with open(f'{OUT}/mab-pairs.jsonl', 'w') as f:
        for p in uniq[:n_pairs]:
            f.write(json.dumps(p) + '\n')
    print('mined', len(pairs), 'unique', len(uniq), 'kept', min(n_pairs, len(uniq)),
          collections.Counter(p['new_fact']['predicate'] for p in uniq[:n_pairs]).most_common(8))


def merge():
    rows = [json.loads(l) for l in open('benchmarks/jev/pairs-handwritten.jsonl')]
    rows += [json.loads(l) for l in open(f'{OUT}/mab-pairs.jsonl')]
    with open(f'{OUT}/pairs.jsonl', 'w') as f:
        for r in rows:
            f.write(json.dumps(r) + '\n')
    print(len(rows), collections.Counter(r['group'] for r in rows))


ADJUDICATION_PROMPT = '''You adjudicate whether a NEW fact contradicts, updates, duplicates, or coexists with EXISTING facts in a memory system.

Decide ONE action:
- "supersede": the new fact is an update/correction — the listed existing fact ids are now outdated.
- "coexist": the new fact and the existing facts are all true simultaneously (different aspects, not contradictory).
- "reject_new": the new fact is redundant with or less reliable than what is stored; do not store it.

Judge by meaning, not wording. An update to a single-valued attribute (city, employer) supersedes. Additional values of a multi-valued attribute (another preference, another project) coexist.

A SET-VALUED attribute — medications, allergies, diagnoses, relationships — coexists unless the text says one ENDED. A second medication is not evidence that the first one stopped. Only a single-valued attribute supersedes.

OUTPUT: one JSON object, nothing else:
{"action":"supersede","ids":["<id>", ...]} or {"action":"coexist"} or {"action":"reject_new"}'''


def chat(messages, retries=5):
    key = os.environ['OPENROUTER_API_KEY']
    body = json.dumps({'model': FACT_MODEL, 'messages': messages, 'temperature': 0.0, 'max_tokens': 2048}).encode()
    for i in range(retries):
        try:
            req = urllib.request.Request(OR_URL, body, {'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
            t = time.time()
            d = json.load(urllib.request.urlopen(req, timeout=120))
            return d['choices'][0]['message']['content'] or '', d.get('usage', {}), time.time() - t
        except urllib.error.HTTPError as e:
            if e.code in (408, 429, 500, 502, 503, 504, 529):
                time.sleep(2 ** i)
                continue
            raise RuntimeError(f'HTTP {e.code}')
        except Exception:
            time.sleep(2 ** i)
    raise RuntimeError('transport')


def parse_action(content):
    t = content.strip()
    s, e = t.find('{'), t.rfind('}')
    try:
        raw = json.loads(t[s:e + 1]) if s >= 0 and e > s else None
    except Exception:
        raw = None
    if not isinstance(raw, dict) or 'action' not in raw:
        return 'unparseable'
    a = raw['action']
    if a == 'supersede':
        return 'supersede' if raw.get('ids') else 'supersede_without_ids'
    return a if a in ('coexist', 'reject_new') else 'unknown_action'


def baseline():
    rows = [json.loads(l) for l in open(f'{OUT}/pairs.jsonl')]
    old_id = '00000000-0000-4000-8000-000000000001'

    def job(r):
        nf, ex = r['new_fact'], r['existing']
        user = f"NEW fact:\n{nf['subject']} {nf['predicate']} {nf['object']}\n\nEXISTING facts:\n{old_id} | {ex['subject']} {ex['predicate']} {ex['object']}"
        content, usage, lat = chat([{'role': 'system', 'content': ADJUDICATION_PROMPT}, {'role': 'user', 'content': user}])
        return {'id': r['id'], 'group': r['group'], 'gold': r['gold']['action'], 'llm_action': parse_action(content),
                'prompt_tokens': usage.get('prompt_tokens', 0), 'completion_tokens': usage.get('completion_tokens', 0), 'latency_s': round(lat, 3)}

    with cf.ThreadPoolExecutor(8) as ex:
        out = list(ex.map(job, rows))
    with open(f'{OUT}/baseline-llm.jsonl', 'w') as f:
        for o in out:
            f.write(json.dumps(o) + '\n')
    cm = collections.Counter((o['gold'], o['llm_action']) for o in out)
    acc = sum(o['gold'] == o['llm_action'] for o in out) / len(out)
    lat = sorted(o['latency_s'] for o in out)
    print('accuracy', round(acc, 3), 'n', len(out), 'p50', lat[len(lat) // 2], 'p95', lat[int(len(lat) * .95)])
    for k in sorted(cm):
        print(' ', k, cm[k])
    by_group = collections.defaultdict(lambda: [0, 0])
    for o in out:
        by_group[o['group']][0] += o['gold'] == o['llm_action']
        by_group[o['group']][1] += 1
    print({g: f'{a}/{b}' for g, (a, b) in sorted(by_group.items())})


if __name__ == '__main__':
    {'mine': mine, 'merge': merge, 'baseline': baseline}[sys.argv[1]]()
