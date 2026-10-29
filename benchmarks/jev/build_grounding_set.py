#!/usr/bin/env python3
"""In-distribution grounding eval set: claims and facts as compile extracts them.

Stages (each writes a JSONL under data/scifact/, gitignored because SciFact is CC-BY-NC):

  extract  - compile's extraction call, verbatim: same system message, same user
             prompt (compile.rs `extraction_prompt`), same default model
             (google/gemini-3.5-flash-lite), temperature 0, over a seeded sample of
             SciFact abstracts disjoint from the human-labelled probe pairs. One
             abstract ~ one compile chunk. Output: every extracted claim and fact,
             BEFORE any grounding gate (compile drops rejected claims, so a normal
             compile run cannot show what the gate threw away).
  label    - an independent judge (anthropic/claude-opus-5.5 via OpenRouter) labels
             each item supported / not_supported with a failure class.
  perturb  - for items Opus labelled supported, Opus writes adversarial variants that
             reuse the passage's words but change the meaning (direction flip,
             negation, hedge removed, population/species swap, association ->
             causation, subject/object swap, prior-study attribution). Labelled
             not_supported by construction; `verify` re-checks a sample independently.
  pairs    - writes grounding-extracted-pairs.jsonl / -docs.jsonl in the shape
             `judge_eval grounding` reads (label SUPPORT | NEI | CONTRADICT, where
             CONTRADICT marks the adversarial perturbations).

Keys come from OPENROUTER_API_KEY and are never printed.
"""
import concurrent.futures as cf, hashlib, json, os, random, re, sys, time, urllib.error, urllib.request, uuid

DATA = os.environ.get('SCIFACT_DIR', 'data/scifact')
OR_URL = 'https://openrouter.ai/api/v1/chat/completions'
EXTRACT_MODEL = 'google/gemini-3.5-flash-lite'   # openrouter.rs DEFAULT_EXTRACTION_MODEL
LABEL_MODEL = 'anthropic/claude-opus-5.5'
KB = 'scifact-eval'
NS = uuid.UUID('6f0a1c9e-3b5d-4e0f-9a1b-2c3d4e5f6a7b')

EXTRACT_SYSTEM = 'Extract cited scientific/domain evidence. Return one JSON object only.'


def extraction_prompt(chunk_id, content):
    # Verbatim from crates/engram-cli/src/commands/compile.rs `extraction_prompt`.
    return (
        f"KB: {KB}\nChunk id: {chunk_id}\nDocument id: none\nSection: unknown\nSource: unknown\n\n"
        "Extract evidence from the chunk below.\nReturn JSON only with this shape:\n"
        '{"claims":[{"content":"short factual claim with numbers if present","evidence_level":"review|meta_analysis|rct|human_observational|animal|in_vitro|preprint|news|opinion|unknown","confidence":0.0}],'
        '"entities":[{"name":"canonical entity","kind":"person|organization|gene_or_protein|drug|place|event|method|species|dataset|concept","aliases":["other spellings of the SAME entity in this chunk"]}],'
        '"relations":[{"from":"entity","to":"entity","predicate":"typed_relation","weight":1.0,"evidence":"the sentence this relation came from"}],'
        '"takeaways":["short cited takeaway"],'
        '"facts":[{"subject":"entity","predicate":"snake_case_relation","object":"value or entity","confidence":0.0}]}\n'
        'People matter: type every human as "person", give their full name as `name` and shorter forms (first name only, possessive, titled) as `aliases`. '
        'Never emit a pronoun, contraction ("She\'s", "I\'m"), weekday, month, or bare common word as an entity.\n'
        'For two people, prefer these predicates: introduced_by (from = the person who was introduced, to = the introducer), friend_of, married_to, dating, family_of, works_with, colleague_of, investor_in, client_of, met_at, knows.\n'
        'Every claim and fact must be directly supported by this chunk. Emit a fact only for a stable, checkable assertion (a dosage, a target, an effect direction), not vague prose. Do not invent citations.\n\n'
        f"Chunk:\n{content}"
    )


def chat(model, messages, max_tokens=4096, retries=5):
    key = os.environ['OPENROUTER_API_KEY']
    body = json.dumps({'model': model, 'messages': messages, 'temperature': 0.0, 'max_tokens': max_tokens}).encode()
    err = None
    for i in range(retries):
        try:
            req = urllib.request.Request(OR_URL, body, {'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
            d = json.load(urllib.request.urlopen(req, timeout=180))
            return d['choices'][0]['message']['content'] or '', d.get('usage', {})
        except urllib.error.HTTPError as e:
            err = f'HTTP {e.code}'
            if e.code in (408, 429, 500, 502, 503, 504, 529):
                time.sleep(2 ** i)
                continue
            raise RuntimeError(err)
        except Exception as e:  # transport
            err = type(e).__name__
            time.sleep(2 ** i)
    raise RuntimeError(err)


def first_json(text, opener='{'):
    t = text.strip()
    t = re.sub(r'^```(?:json)?\s*', '', t)
    t = re.sub(r'\s*```$', '', t)
    closer = '}' if opener == '{' else ']'
    start = t.find(opener)
    if start < 0:
        raise ValueError('no json object')
    depth, in_str, esc = 0, False, False
    for j in range(start, len(t)):
        c = t[j]
        if in_str:
            if esc:
                esc = False
            elif c == '\\':
                esc = True
            elif c == '"':
                in_str = False
            continue
        if c == '"':
            in_str = True
        elif c == opener:
            depth += 1
        elif c == closer:
            depth -= 1
            if depth == 0:
                return json.loads(t[start:j + 1])
    raise ValueError('no balanced json')


def load_jsonl(path):
    return [json.loads(l) for l in open(path)]


def write_jsonl(path, rows):
    with open(path, 'w') as f:
        for r in rows:
            f.write(json.dumps(r) + '\n')


# ---- extract ------------------------------------------------------------------

def extract(n_docs=100, seed=20260925):
    probe_docs = {d['doc_id'] for d in load_jsonl(f'{DATA}/grounding-scifact-docs.jsonl')}
    corpus = [d for d in load_jsonl(f'{DATA}/corpus.jsonl') if d['doc_id'] not in probe_docs]
    random.Random(seed).shuffle(corpus)
    docs = corpus[:n_docs]
    out_docs = [{'doc_id': d['doc_id'], 'title': d['title'], 'text': ' '.join(d['abstract'])} for d in docs]
    write_jsonl(f'{DATA}/extracted-docs.jsonl', out_docs)

    def job(d):
        content = d['title'] + '. ' + d['text']
        chunk_id = str(uuid.uuid5(NS, str(d['doc_id'])))
        text, usage = chat(EXTRACT_MODEL, [{'role': 'system', 'content': EXTRACT_SYSTEM},
                                           {'role': 'user', 'content': extraction_prompt(chunk_id, content)}], max_tokens=2048)
        try:
            ev = first_json(text)
        except Exception:
            return d['doc_id'], [], usage, 'parse_failed'
        items = []
        for k, c in enumerate(ev.get('claims') or []):
            if isinstance(c, dict) and (c.get('content') or '').strip():
                items.append({'id': f"{d['doc_id']}-c{k}", 'doc_id': d['doc_id'], 'kind': 'claim',
                              'claim': c['content'].strip(), 'evidence_level': c.get('evidence_level')})
        for k, f in enumerate(ev.get('facts') or []):
            if isinstance(f, dict) and all((f.get(x) or '').strip() for x in ('subject', 'predicate', 'object')):
                items.append({'id': f"{d['doc_id']}-f{k}", 'doc_id': d['doc_id'], 'kind': 'fact',
                              'subject': f['subject'].strip(), 'predicate': f['predicate'].strip(), 'object': f['object'].strip(),
                              # Rendered the way questions::fact_as_claim renders it.
                              'claim': f"{f['subject'].strip()} {f['predicate'].strip().replace('_', ' ')} {f['object'].strip()}"})
        return d['doc_id'], items, usage, None

    items, pt, ct, failed = [], 0, 0, 0
    with cf.ThreadPoolExecutor(8) as ex:
        for doc_id, its, usage, err in ex.map(job, out_docs):
            items.extend(its)
            pt += usage.get('prompt_tokens', 0)
            ct += usage.get('completion_tokens', 0)
            failed += err is not None
    write_jsonl(f'{DATA}/extracted-items.jsonl', items)
    kinds = {k: sum(1 for i in items if i['kind'] == k) for k in ('claim', 'fact')}
    print(f'docs {len(out_docs)} parse_failed {failed} items {kinds} prompt_tokens {pt} completion_tokens {ct}')


# ---- label --------------------------------------------------------------------

LABEL_SYSTEM = """You are a strict evidence checker for a medical and longevity knowledge base, where admitting a false claim is worse than rejecting a true one.
For each numbered item, decide whether the PASSAGE supports it.
"supported": the passage states it or directly implies it, with the same direction of effect, the same subject and object, the same population or species, the same numbers, and no stronger certainty than the passage. A faithful paraphrase counts. For facts written as "subject predicate object", read them as a plain statement.
"not_supported": anything else. Give the failure class: direction_flip, negation, hedge_removed, population_or_species, association_to_causation, subject_object_swap, prior_study_attribution, wrong_number, not_stated, too_vague_or_malformed, other.
Return ONE JSON object only: {"items":[{"n":1,"label":"supported"|"not_supported","class":"<class or none>","note":"<=15 words"}]}"""


def label_items(items_path, out_path, docs_path):
    docs = {d['doc_id']: d for d in load_jsonl(docs_path)}
    items = load_jsonl(items_path)
    by_doc = {}
    for it in items:
        by_doc.setdefault(it['doc_id'], []).append(it)

    def job(doc_id):
        its = by_doc[doc_id]
        d = docs[doc_id]
        listing = '\n'.join(f"{n + 1}. {it['claim']}" for n, it in enumerate(its))
        msg = f"PASSAGE:\n{d['title']}. {d['text']}\n\nITEMS:\n{listing}"
        got, usage = {}, {}
        for _ in range(2):  # one retry on an unparseable reply; unlabelled items stay None
            text, usage = chat(LABEL_MODEL, [{'role': 'system', 'content': LABEL_SYSTEM}, {'role': 'user', 'content': msg}], max_tokens=4096)
            try:
                got = {int(x['n']): x for x in first_json(text)['items']}
                break
            except Exception:
                continue
        out = []
        for n, it in enumerate(its):
            g = got.get(n + 1)
            out.append({**it, 'opus_label': g['label'] if g else None, 'opus_class': g.get('class') if g else None,
                        'opus_note': g.get('note') if g else None})
        return out, usage

    rows, pt, ct = [], 0, 0
    with cf.ThreadPoolExecutor(6) as ex:
        for out, usage in ex.map(job, list(by_doc)):
            rows.extend(out)
            pt += usage.get('prompt_tokens', 0)
            ct += usage.get('completion_tokens', 0)
    write_jsonl(out_path, rows)
    lab = {}
    for r in rows:
        lab[(r['kind'], r['opus_label'])] = lab.get((r['kind'], r['opus_label']), 0) + 1
    print(f'labelled {len(rows)} {lab} prompt_tokens {pt} completion_tokens {ct}')


# ---- perturb ------------------------------------------------------------------

PERTURB_SYSTEM = """You build adversarial test cases for a claim-grounding checker in a medical knowledge base.
Given a PASSAGE and claims the passage SUPPORTS, write variants of each claim that REUSE MOST OF THE CLAIM'S WORDS but are NOT supported by the passage, one per applicable class:
- direction_flip: reverse the direction of an effect (increased <-> decreased, higher <-> lower, improved <-> worsened).
- negation: add or remove a negation so the meaning flips.
- hedge_removed: turn a hedged or tentative finding (may, might, suggests, associated with, in some) into a definite one. Only if the passage hedges.
- population_or_species: move the finding to a different population or species (mice -> humans, cells -> patients, adults -> children). Only if the passage names one.
- association_to_causation: turn a correlation or association into a causal claim. Only if the passage reports an association.
- subject_object_swap: swap the roles of two entities (A inhibits B -> B inhibits A).
- prior_study_attribution: present a finding the passage attributes to earlier/cited work as this study's own result. Only if the passage cites prior work.
Skip a class when it does not apply. Each variant must be clearly false or unsupported given the passage, and must read naturally.
Return ONE JSON object only: {"variants":[{"source":<claim number>,"class":"<class>","claim":"<variant>"}]}"""


def perturb(labelled_path, out_path, docs_path, per_doc=3):
    docs = {d['doc_id']: d for d in load_jsonl(docs_path)}
    rows = load_jsonl(labelled_path)
    by_doc = {}
    for r in rows:
        if r['kind'] == 'claim' and r['opus_label'] == 'supported':
            by_doc.setdefault(r['doc_id'], []).append(r)

    def job(doc_id):
        src = by_doc[doc_id][:per_doc]
        d = docs[doc_id]
        listing = '\n'.join(f"{n + 1}. {s['claim']}" for n, s in enumerate(src))
        variants, usage = [], {}
        for _ in range(2):  # one retry on an empty or unparseable reply
            text, usage = chat(LABEL_MODEL, [{'role': 'system', 'content': PERTURB_SYSTEM},
                                             {'role': 'user', 'content': f"PASSAGE:\n{d['title']}. {d['text']}\n\nSUPPORTED CLAIMS:\n{listing}"}], max_tokens=4096)
            try:
                variants = first_json(text).get('variants') or []
                break
            except Exception:
                continue
        out = []
        for k, v in enumerate(variants):
            if not isinstance(v, dict):
                continue
            try:
                s = src[int(v['source']) - 1]
            except Exception:
                continue
            out.append({'id': f"{s['id']}-p{k}", 'doc_id': doc_id, 'kind': 'perturbation', 'class': v.get('class'),
                        'claim': v['claim'], 'source_id': s['id'], 'source_claim': s['claim']})
        return out, usage

    out_rows, pt, ct = [], 0, 0
    with cf.ThreadPoolExecutor(6) as ex:
        for out, usage in ex.map(job, list(by_doc)):
            out_rows.extend(out)
            pt += usage.get('prompt_tokens', 0)
            ct += usage.get('completion_tokens', 0)
    write_jsonl(out_path, out_rows)
    cls = {}
    for r in out_rows:
        cls[r['class']] = cls.get(r['class'], 0) + 1
    print(f'perturbations {len(out_rows)} {cls} prompt_tokens {pt} completion_tokens {ct}')


# ---- pairs --------------------------------------------------------------------

def pairs():
    labelled = load_jsonl(f'{DATA}/extracted-labelled.jsonl')
    perturbed = load_jsonl(f'{DATA}/extracted-perturbed-labelled.jsonl')
    out = []
    for r in labelled:
        if r['opus_label'] not in ('supported', 'not_supported'):
            continue
        out.append({'id': r['id'], 'doc_id': r['doc_id'], 'claim': r['claim'], 'kind': r['kind'],
                    'label': 'SUPPORT' if r['opus_label'] == 'supported' else 'NEI', 'class': r.get('opus_class')})
    for r in perturbed:
        # A perturbation Opus still calls supported is not a clean negative; drop it.
        if r.get('opus_label') == 'supported':
            continue
        out.append({'id': r['id'], 'doc_id': r['doc_id'], 'claim': r['claim'], 'kind': 'perturbation',
                    'label': 'CONTRADICT', 'class': r['class']})
    write_jsonl(f'{DATA}/grounding-extracted-pairs.jsonl', out)
    import collections
    print(len(out), collections.Counter((p['kind'], p['label']) for p in out))


if __name__ == '__main__':
    cmd = sys.argv[1]
    if cmd == 'extract':
        extract(int(sys.argv[2]) if len(sys.argv) > 2 else 100)
    elif cmd == 'label':
        label_items(f'{DATA}/extracted-items.jsonl', f'{DATA}/extracted-labelled.jsonl', f'{DATA}/extracted-docs.jsonl')
    elif cmd == 'perturb':
        perturb(f'{DATA}/extracted-labelled.jsonl', f'{DATA}/extracted-perturbed.jsonl', f'{DATA}/extracted-docs.jsonl')
    elif cmd == 'verify':
        # Independent re-check: Opus labels every perturbation against the passage without being told it was perturbed.
        label_items(f'{DATA}/extracted-perturbed.jsonl', f'{DATA}/extracted-perturbed-labelled.jsonl', f'{DATA}/extracted-docs.jsonl')
    elif cmd == 'pairs':
        pairs()
