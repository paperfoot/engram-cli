#!/usr/bin/env python3
"""Build a grounded QA test set from real ingested passages in a engram KB.
Each question is generated from one real chunk; its source paper is the retrieval ground truth."""
import os, json, sqlite3, urllib.request, sys, time, re

DB = "/Users/dev/Library/Application Support/bio.199-biotechnologies.engram/db.sqlite"
OUT = "/Users/dev/projects/engram/benchmarks/bakeoff/testset_tms.json"
KB = os.environ.get("BAKEOFF_KB", "tms")
N_TARGET = int(os.environ.get("BAKEOFF_N", "24"))
N_SAMPLE = N_TARGET * 2
ORKEY = os.environ["OPENROUTER_API_KEY"]
QGEN_MODEL = "google/gemini-3.1-flash-lite-preview"

def orchat(model, messages, temperature=0.2, max_tokens=700):
    body = json.dumps({"model": model, "messages": messages,
                       "temperature": temperature, "max_tokens": max_tokens}).encode()
    req = urllib.request.Request("https://openrouter.ai/api/v1/chat/completions", data=body,
        headers={"Authorization": f"Bearer {ORKEY}", "Content-Type": "application/json",
                 "X-Title": "engram-bakeoff"})
    last = None
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                return json.loads(r.read())["choices"][0]["message"]["content"]
        except Exception as e:
            last = e; time.sleep(2 * (attempt + 1))
    raise last

def extract_json(txt):
    m = re.search(r'\{.*\}', txt, re.S)
    if not m: return None
    try: return json.loads(m.group(0))
    except: return None

c = sqlite3.connect(DB)
rows = c.execute(f"""
  SELECT ch.id, ch.content, ch.document_id, d.title, d.source_path
  FROM chunks ch JOIN documents d ON ch.document_id = d.id
  WHERE d.kb=? AND length(ch.content) BETWEEN 800 AND 2400
  GROUP BY ch.document_id
  ORDER BY RANDOM() LIMIT ?""", (KB, N_SAMPLE)).fetchall()

items = []
for (cid, content, docid, title, sp) in rows:
    if len(items) >= N_TARGET: break
    prompt = [
      {"role": "system", "content":
       "You write evaluation questions for a scientific-literature QA system. Given a passage from a "
       "paper, produce ONE specific, factual question answerable ONLY from this passage, plus the exact "
       "answer. Prefer concrete facts: numbers, effect sizes, mechanisms, specific findings, stimulation "
       "parameters, sample sizes. Avoid vague questions. The question must be self-contained (name the "
       "topic/population so it makes sense without the passage). Output STRICT JSON: "
       "{\"question\":\"...\",\"answer\":\"...\",\"specific\":true|false}. specific=false if the passage "
       "has no concrete testable fact."},
      {"role": "user", "content": f"Passage:\n\"\"\"\n{content[:2200]}\n\"\"\""}
    ]
    try:
        txt = orchat(QGEN_MODEL, prompt)
    except Exception as e:
        print("qgen fail:", e, file=sys.stderr); continue
    j = extract_json(txt)
    if not j or not j.get("specific") or not j.get("question") or not j.get("answer"):
        continue
    pmid = ""
    if sp:
        mm = re.search(r'PMID(\d+)', sp)
        if mm: pmid = mm.group(1)
    items.append({
        "id": f"{KB}-{len(items)+1:02d}",
        "question": j["question"].strip(),
        "expected_answer": j["answer"].strip(),
        "source_doc_id": docid,
        "source_title": title,
        "source_pmid": pmid,
        "source_path": sp,
        "source_chunk_id": cid,
        "source_chunk": content[:2200],
    })
    print(f"  built {items[-1]['id']} pmid={pmid} :: {items[-1]['question'][:72]}", file=sys.stderr)

out = {"meta": {"kb": KB, "n": len(items), "qgen_model": QGEN_MODEL,
                "note": "Questions generated from real ingested passages; source paper = retrieval ground truth."},
       "questions": items}
os.makedirs(os.path.dirname(OUT), exist_ok=True)
json.dump(out, open(OUT, "w"), indent=2)
print(f"WROTE {len(items)} questions -> {OUT}")
