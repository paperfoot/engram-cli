#!/usr/bin/env python3
"""Fit and test the conflict rule engram ships (`fact_write::conflict_rule`).

  python3 benchmarks/jev/adjudicate_rule.py PAIRS.jsonl SIGNALS.jsonl LLM.jsonl \
      [--seed 0] [--splits 50] [--scope all|engram] [--save AGG.json]

PAIRS: the labelled pair set (`{id, group, gold{action}}`, e.g.
pairs-handwritten.jsonl + the mined MAB pairs). SIGNALS: `judge_eval pairs`
output (`{id, restates, same_slot, contradicts, old_ended}`). LLM: the
prompted adjudicator's verbatim answers on the same ids (`{id, llm_action}`).

The rule maps four JEV probabilities to one of the adjudicator's outcomes:

    restates    >= R            -> reject_new   (restatement: dedupe)
    same_slot   <  S            -> coexist      (different attributes)
    contradicts >= C
      and old_ended >= E        -> supersede    (E is a direction check: the
                                                 text must say the OLD value
                                                 ended, not the new one)
    contradicts <  L            -> coexist      (compatible values)
    otherwise                   -> queue        (the agent decides)

Thresholds are fitted on a random half (stratified by group) and scored on the
OTHER half; `queue` counts as not correct. Ties on the fit half go to fewer
wrong automatic actions, then to fewer queued pairs, then to the highest bars
for discarding and closing. The LLM is scored on the
same held-out ids. `--splits` repeats the whole fit/test over more seeds to
show how much one split's number moves.

Safety groups (`safety_*`) are reported apart: engram never auto-closes,
clears or discards a safety pair whatever the rule says (`may_close` and the
safety guard in `apply_rule`), so their "engine" outcome is always `queue`.

`--scope engram` keeps only the pairs engram's planner can hand to the
adjudicator: a safety pair, or a pair whose relations are both single-valued in
the registry (`engram_core::relations`, mirrored below). A pair on a set-valued
relation (most mined MAB predicates: citizen_of, speaks_language, ...) never
reaches it: the planner stores both values (`multi_valued`) by rule.

The saved aggregate holds numbers only, never fact text.
"""
import collections, itertools, json, random, sys

R_GRID = [0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9]
S_GRID = [0.05, 0.1, 0.2, 0.3, 0.5]
E_GRID = [0.0, 0.05, 0.1, 0.15, 0.2, 0.3, 0.5]
C_GRID = [0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9]
L_GRID = [0.05, 0.1, 0.2, 0.3, 0.4]
SIGNALS = ['restates', 'same_slot', 'contradicts', 'old_ended']

# Mirror of engram_core::relations::{canonical_relation, relation_spec}.
SYNONYMS = {
    'employed_by': 'works_at', 'employer': 'works_at', 'job_at': 'works_at',
    'works_for': 'works_at', 'employed_at': 'works_at', 'resides_in': 'lives_in',
    'home_city': 'lives_in', 'based_in': 'lives_in', 'living_in': 'lives_in',
    'residence': 'lives_in', 'age': 'has_age', 'aged': 'has_age', 'is_age': 'has_age',
    'role': 'has_role', 'position': 'has_role', 'job_title': 'has_role', 'title': 'has_role',
    'spouse': 'married_to', 'spouse_of': 'married_to', 'wife_of': 'married_to',
    'husband_of': 'married_to', 'dating': 'partner_of', 'girlfriend_of': 'partner_of',
    'boyfriend_of': 'partner_of', 'in_relationship_with': 'partner_of',
    'allergy': 'allergic_to', 'has_allergy': 'allergic_to', 'allergic': 'allergic_to',
    'medication': 'takes_medication', 'on_medication': 'takes_medication',
    'takes_drug': 'takes_medication', 'prescribed': 'takes_medication',
    'favorite_language': 'prefers_language', 'preferred_language': 'prefers_language',
    'codes_in': 'prefers_language',
}
SAFETY = {'takes_medication', 'allergic_to', 'diagnosed_with', 'blood_type'}
SINGLE = {'blood_type', 'birthday', 'has_birthday', 'date_of_birth', 'born_on', 'died_on',
          'nationality', 'has_nationality', 'lives_in', 'works_at', 'has_role', 'has_age',
          'married_to', 'partner_of', 'prefers_language'}


def canonical(p):
    p = p.strip().lower().replace(' ', '_').replace('-', '_')
    return SYNONYMS.get(p, p)


def engram_adjudicates(row):
    a, b = canonical(row['new_fact']['predicate']), canonical(row['existing']['predicate'])
    return a in SAFETY or b in SAFETY or (a in SINGLE and b in SINGLE)


def rule(s, t):
    R, S, E, C, L = t
    if s['restates'] >= R:
        return 'reject_new'
    if s['same_slot'] < S:
        return 'coexist'
    if s['contradicts'] >= C and s['old_ended'] >= E:
        return 'supersede'
    if s['contradicts'] < L:
        return 'coexist'
    return 'queue'


def score(items, t):
    correct = wrong = queued = 0
    for it in items:
        a = rule(it['sig'], t)
        if a == it['gold']:
            correct += 1
        elif a == 'queue':
            queued += 1
        else:
            wrong += 1
    return correct, wrong, queued


def fit(items):
    best, best_key = None, None
    for t in itertools.product(R_GRID, S_GRID, E_GRID, C_GRID, L_GRID):
        if t[4] > t[3]:
            continue
        c, w, q = score(items, t)
        # Among ties, the most conservative cut: higher bars to discard (R)
        # and to close (E, C) first.
        key = (c, -w, -q, t[0] + t[2] + t[3])
        if best_key is None or key > best_key:
            best, best_key = t, key
    return best


def split(items, seed):
    by_group = collections.defaultdict(list)
    for it in items:
        by_group[it['group']].append(it)
    rng = random.Random(seed)
    a, b = [], []
    for g in sorted(by_group):
        xs = sorted(by_group[g], key=lambda x: x['id'])
        rng.shuffle(xs)
        # Odd groups alternate which half gets the extra item.
        cut = len(xs) // 2 + (len(xs) % 2 if (seed + len(a)) % 2 == 0 else 0)
        a += xs[:cut]
        b += xs[cut:]
    return a, b


def is_safety(it):
    return it['group'].startswith('safety')


def report(fit_items, test_items, t):
    def rows(xs):
        c, w, q = score(xs, t)
        llm = sum(1 for x in xs if x['llm'] == x['gold'])
        return {'n': len(xs), 'rule_correct': c, 'rule_wrong_auto': w, 'rule_queued': q,
                'rule_acc': round(c / max(len(xs), 1), 4), 'llm_correct': llm,
                'llm_acc': round(llm / max(len(xs), 1), 4)}
    safety = [x for x in test_items if is_safety(x)]
    return {
        'thresholds': dict(zip('RSECL', t)),
        'fit': rows(fit_items),
        'held_out': rows(test_items),
        'held_out_routine': rows([x for x in test_items if not is_safety(x)]),
        'held_out_safety': rows(safety),
        # What the RULE would close on a safety pair before engram's guard.
        'safety_rule_supersede': sum(1 for x in safety if rule(x['sig'], t) == 'supersede'),
        'safety_engine_auto_closed': 0,
    }


def main(argv):
    if len(argv) < 3:
        sys.exit(__doc__)
    flags = dict(zip(argv[3::2], argv[4::2]))
    gold = {}
    for path in argv[0].split(','):
        for line in open(path):
            if line.strip():
                r = json.loads(line)
                if flags.get('--scope') == 'engram' and not engram_adjudicates(r):
                    continue
                gold[r['id']] = (r['group'], r['gold']['action'])
    sig = {}
    for line in open(argv[1]):
        r = json.loads(line)
        if r.get('summary') or any(r.get(k) is None for k in SIGNALS):
            continue
        sig[r['id']] = {k: r[k] for k in SIGNALS}
    llm = {}
    for line in open(argv[2]):
        r = json.loads(line)
        llm[r['id']] = r['llm_action']
    items = [{'id': i, 'group': g, 'gold': a, 'sig': sig[i], 'llm': llm.get(i, 'missing')}
             for i, (g, a) in gold.items() if i in sig]
    missing = len(gold) - len(items)

    seed = int(flags.get('--seed', 0))
    fit_items, test_items = split(items, seed)
    t = fit(fit_items)
    primary = report(fit_items, test_items, t)

    reps = []
    for s in range(int(flags.get('--splits', 50))):
        a, b = split(items, 1000 + s)
        r = report(a, b, fit(a))['held_out']
        reps.append((r['rule_acc'], r['llm_acc'], r['rule_wrong_auto'], r['n']))
    mean = lambda xs: round(sum(xs) / len(xs), 4)
    out = {
        'scope': flags.get('--scope', 'all'),
        'items': len(items), 'missing_signals': missing, 'seed': seed,
        'primary_split': primary,
        'repeated_splits': {
            'n': len(reps),
            'pooled_rule_acc': round(sum(r[0] * r[3] for r in reps) / sum(r[3] for r in reps), 4),
            'pooled_llm_acc': round(sum(r[1] * r[3] for r in reps) / sum(r[3] for r in reps), 4),
            'rule_acc_mean': mean([r[0] for r in reps]),
            'rule_acc_min': min(r[0] for r in reps),
            'llm_acc_mean': mean([r[1] for r in reps]),
            'rule_ge_llm': sum(1 for r in reps if r[0] >= r[1]),
            'rule_wrong_auto_mean': mean([r[2] for r in reps]),
        },
    }
    print(json.dumps(out, indent=2))
    if '--save' in flags:
        json.dump(out, open(flags['--save'], 'w'), indent=2)


if __name__ == '__main__':
    main(sys.argv[1:])
