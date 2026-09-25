#!/usr/bin/env python3
"""Grounding eval for `remember`'s own fact extraction (the surface that survives the roadmap).

  extract - remember's extraction call, verbatim: facts.rs FACT_EXTRACTION_PROMPT +
            extraction_system_prompt's DATE CONTEXT as the system message, the memory
            text as the user message, default fact model google/gemini-3.5-flash-lite,
            temperature 0. Memories: seeded 8-turn windows of LoCoMo conversations
            (personal chat) plus benchmarks/jev/remember-health-notes.jsonl (hand-written
            health notes full of negations, hedges and cessations, generic names).
  label   - Opus 5.5 labels each extracted triple against its memory text; an
            "ended" triple is read as "the state stopped".
  pairs   - grounding pairs for `judge_eval grounding`, one per fact, in two renderings:
            plain ("S predicate O") and state-aware ("S no longer predicate O" when ended).

Data goes to data/pairs/ (gitignored). OPENROUTER_API_KEY from the environment, never printed.
"""
import collections, concurrent.futures as cf, json, os, random, sys
sys.path.insert(0, os.path.dirname(__file__))
from build_grounding_set import chat, first_json, load_jsonl, write_jsonl, LABEL_MODEL  # noqa: E402

OUT = 'data/pairs'
LOCOMO = 'data/locomo/locomo10.json'
FACT_MODEL = 'google/gemini-3.5-flash-lite'


def fact_prompt_text():
    src = open('crates/engram-graph/src/facts.rs').read()
    start = src.index('const FACT_EXTRACTION_PROMPT: &str = r#"') + len('const FACT_EXTRACTION_PROMPT: &str = r#"')
    return src[start:src.index('"#;', start)]


def system_prompt(date):
    return (fact_prompt_text() + "\n\nDATE CONTEXT:\nThis memory is dated " + date + ". Resolve every relative expression "
            "(\"last March\", \"two years ago\", \"since the summer\") against that date and emit valid_from as an absolute "
            "YYYY-MM-DD. If you cannot resolve one to an absolute date, emit null — an omitted date is inherited from the "
            "memory, a guessed one silently overwrites true history.")


def memories(n_windows=60, seed=20260925):
    convs = json.load(open(LOCOMO))
    windows = []
    for ci, c in enumerate(convs):
        conv = c['conversation']
        for k in conv:
            if k.startswith('session_') and not k.endswith('date_time') and isinstance(conv[k], list):
                turns = conv[k]
                for s in range(0, max(1, len(turns) - 7), 8):
                    text = '\n'.join(f"{t['speaker']}: {t['text']}" for t in turns[s:s + 8])
                    windows.append({'mem_id': f'locomo{ci}-{k}-{s}', 'text': text, 'date': '2023-06-01', 'source': 'locomo'})
    random.Random(seed).shuffle(windows)
    notes = load_jsonl('benchmarks/jev/remember-health-notes.jsonl')
    return windows[:n_windows] + [{**n, 'source': 'health_note'} for n in notes]


def extract():
    mems = memories()
    write_jsonl(f'{OUT}/remember-memories.jsonl', mems)

    def job(m):
        text, usage = chat(FACT_MODEL, [{'role': 'system', 'content': system_prompt(m['date'])}, {'role': 'user', 'content': m['text']}], max_tokens=2048)
        try:
            facts = first_json(text, '[')
        except Exception:
            return m, [], usage, True
        return m, [f for f in facts if isinstance(f, dict) and all(str(f.get(x) or '').strip() for x in ('subject', 'predicate', 'object'))], usage, False

    items, failed = [], 0
    with cf.ThreadPoolExecutor(8) as ex:
        for m, facts, usage, bad in ex.map(job, mems):
            failed += bad
            for k, f in enumerate(facts):
                items.append({'id': f"{m['mem_id']}-f{k}", 'doc_id': m['mem_id'], 'source': m['source'],
                              'subject': str(f['subject']).strip(), 'predicate': str(f['predicate']).strip(), 'object': str(f['object']).strip(),
                              'state': f.get('state') or 'ongoing'})
    write_jsonl(f'{OUT}/remember-items.jsonl', items)
    print('memories', len(mems), 'parse_failed', failed, 'facts', len(items), collections.Counter((i['source'], i['state']) for i in items))


LABEL_SYSTEM = """You are a strict checker for a personal memory store that also holds health information, where storing a false fact is worse than dropping a true one.
For each numbered triple, decide whether the MEMORY TEXT supports it. Read "subject predicate object" as a plain statement; a triple marked [ENDED] claims the state STOPPED (for example "[ENDED] Ana takes_medication metformin" means Ana stopped taking metformin).
"supported": the text states it or directly implies it, including who it is about, negation, hedging (a hedged or merely considered statement is NOT support for a definite one), and whether the state is current or ended.
"not_supported": anything else. Class: wrong_subject, negation, hedge_removed, ended_vs_ongoing, not_stated, wrong_value, too_vague_or_malformed, other.
Return ONE JSON object only: {"items":[{"n":1,"label":"supported"|"not_supported","class":"<class or none>","note":"<=15 words"}]}"""


def label():
    mems = {m['mem_id']: m for m in load_jsonl(f'{OUT}/remember-memories.jsonl')}
    items = load_jsonl(f'{OUT}/remember-items.jsonl')
    by = collections.defaultdict(list)
    for it in items:
        by[it['doc_id']].append(it)

    def render(it):
        return ('[ENDED] ' if it['state'] == 'ended' else '') + f"{it['subject']} {it['predicate']} {it['object']}"

    def job(mid):
        its = by[mid]
        listing = '\n'.join(f'{n + 1}. {render(it)}' for n, it in enumerate(its))
        got = {}
        for _ in range(2):
            text, _usage = chat(LABEL_MODEL, [{'role': 'system', 'content': LABEL_SYSTEM},
                                              {'role': 'user', 'content': f"MEMORY TEXT:\n{mems[mid]['text']}\n\nTRIPLES:\n{listing}"}], max_tokens=4096)
            try:
                got = {int(x['n']): x for x in first_json(text)['items']}
                break
            except Exception:
                continue
        return [{**it, 'opus_label': (got.get(n + 1) or {}).get('label'), 'opus_class': (got.get(n + 1) or {}).get('class')} for n, it in enumerate(its)]

    rows = []
    with cf.ThreadPoolExecutor(6) as ex:
        for out in ex.map(job, list(by)):
            rows.extend(out)
    write_jsonl(f'{OUT}/remember-labelled.jsonl', rows)
    print(collections.Counter((r['source'], r['opus_label']) for r in rows))


def pairs():
    mems = load_jsonl(f'{OUT}/remember-memories.jsonl')
    write_jsonl(f'{OUT}/remember-docs.jsonl', [{'doc_id': m['mem_id'], 'title': '', 'text': m['text']} for m in mems])
    rows = [r for r in load_jsonl(f'{OUT}/remember-labelled.jsonl') if r['opus_label'] in ('supported', 'not_supported')]
    for variant in ('plain', 'state'):
        out = []
        for r in rows:
            pred = r['predicate'].replace('_', ' ')
            claim = f"{r['subject']} {pred} {r['object']}"
            if variant == 'state' and r['state'] == 'ended':
                claim = f"{r['subject']} no longer {pred} {r['object']}"
            out.append({'id': r['id'], 'doc_id': r['doc_id'], 'claim': claim, 'source': r['source'], 'state': r['state'],
                        'label': 'SUPPORT' if r['opus_label'] == 'supported' else 'NEI', 'class': r['opus_class']})
        write_jsonl(f'{OUT}/remember-pairs-{variant}.jsonl', out)
    print(len(rows), collections.Counter((r['source'], r['state'], r['opus_label']) for r in rows))


if __name__ == '__main__':
    {'extract': extract, 'label': label, 'pairs': pairs}[sys.argv[1]]()
