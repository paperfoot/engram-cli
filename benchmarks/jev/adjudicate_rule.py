#!/usr/bin/env python3
"""Fit and test the conflict rule engram ships (`fact_write::conflict_rule`).

  python3 benchmarks/jev/adjudicate_rule.py PAIRS.jsonl SIGNALS.jsonl LLM.jsonl \
      [--seed 0] [--splits 50] [--save AGG.json]

PAIRS: the labelled pair set (`{id, group, gold{action}}`, e.g.
pairs-handwritten.jsonl + the mined MAB pairs). SIGNALS: `judge_eval pairs`
output (`{id, restates, same_slot, contradicts, old_ended}`). LLM: the
prompted adjudicator's verbatim answers on the same ids (`{id, llm_action}`).

The rule maps four JEV probabilities to one of the adjudicator's outcomes:

    restates    >= R            -> reject_new   (restatement: dedupe)
    same_slot   <  S            -> coexist      (different attributes)
    old_ended   >= E
      or contradicts >= C       -> supersede
    contradicts <  L            -> coexist      (compatible values)
    otherwise                   -> queue        (the agent decides)

Thresholds are fitted on a random half (stratified by group) and scored on the
OTHER half; `queue` counts as not correct. Ties on the fit half go to fewer
wrong automatic actions, then to fewer queued pairs. The LLM is scored on the
same held-out ids. `--splits` repeats the whole fit/test over more seeds to
show how much one split's number moves.

Safety groups (`safety_*`) are reported apart: engram never auto-closes,
clears or discards a safety pair whatever the rule says (`may_close` and the
safety guard in `apply_rule`), so their "engine" outcome is always `queue`.

The saved aggregate holds numbers only, never fact text.
"""
import collections, itertools, json, random, sys

R_GRID = [0.5, 0.6, 0.7, 0.8, 0.85, 0.9, 0.95]
S_GRID = [0.05, 0.1, 0.2, 0.3, 0.4, 0.5]
E_GRID = [0.5, 0.6, 0.7, 0.8, 0.85, 0.9, 0.95, 1.01]
C_GRID = [0.5, 0.6, 0.7, 0.8, 0.85, 0.9, 0.95]
L_GRID = [0.05, 0.1, 0.2, 0.3, 0.4, 0.5]
SIGNALS = ['restates', 'same_slot', 'contradicts', 'old_ended']


def rule(s, t):
    R, S, E, C, L = t
    if s['restates'] >= R:
        return 'reject_new'
    if s['same_slot'] < S:
        return 'coexist'
    if s['old_ended'] >= E or s['contradicts'] >= C:
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
        key = (c, -w, -q)
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
        reps.append((r['rule_acc'], r['llm_acc'], r['rule_wrong_auto']))
    mean = lambda xs: round(sum(xs) / len(xs), 4)
    out = {
        'items': len(items), 'missing_signals': missing, 'seed': seed,
        'primary_split': primary,
        'repeated_splits': {
            'n': len(reps),
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
