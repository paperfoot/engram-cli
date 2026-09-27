#!/usr/bin/env python3
"""Score `judge_eval` outputs (crates/engram-bench/examples/judge_eval.rs).

  python3 benchmarks/jev/score.py grounding OUT.jsonl [--save AGG.json]
  python3 benchmarks/jev/score.py pairs OUT.jsonl [--save AGG.json]

grounding: "supported" (label SUPPORT) is the positive class; CONTRADICT and
NEI are negatives. Reports precision/recall of supported and the share of
CONTRADICT and NEI pairs admitted, for the judge at several thresholds, the
lexical rule, and numbers_ok + judge; plus AUC for each. An unanswered pair
scores 0.0 (as in scifact_grounding_probe.py) and is counted in `missing`.

pairs: `gold` is an object of signal -> bool (restates, same_slot,
contradicts, old_ended); a signal absent from gold is not scored for that
item. Reports per-signal AUC and a threshold table.

The saved aggregate holds numbers only, never claim or passage text.
"""
import collections, json, os, sys

GROUNDING_THRESHOLDS = [0.3, 0.5, 0.6, 0.7, 0.8, 0.9]
PAIR_THRESHOLDS = [0.1, 0.2, 0.3, 0.5, 0.7, 0.8, 0.85, 0.9, 0.95]
SIGNALS = ['restates', 'same_slot', 'contradicts', 'old_ended']


def load(path):
    rows = [json.loads(l) for l in open(path) if l.strip()]
    summary = next((r for r in rows if r.get('summary')), {})
    return [r for r in rows if not r.get('summary')], summary


def auc(scores, labels):
    """P(score of a random positive > a random negative), ties count half."""
    pos = sorted(s for s, l in zip(scores, labels) if l)
    neg = sorted(s for s, l in zip(scores, labels) if not l)
    if not pos or not neg:
        return None
    # Rank-sum form of the probe's pairwise count, same tie rule, O(n log n).
    ranked = sorted([(s, 1) for s in pos] + [(s, 0) for s in neg])
    rank_sum, i = 0.0, 0
    while i < len(ranked):
        j = i
        while j < len(ranked) and ranked[j][0] == ranked[i][0]:
            j += 1
        mid = (i + 1 + j) / 2  # average 1-based rank of the tie group
        rank_sum += mid * sum(1 for k in range(i, j) if ranked[k][1])
        i = j
    return round((rank_sum - len(pos) * (len(pos) + 1) / 2) / (len(pos) * len(neg)), 4)


def grounding_metrics(pred, labels):
    n = collections.Counter(labels)
    tp = sum(1 for x, l in zip(pred, labels) if x and l == 'SUPPORT')
    fp = sum(1 for x, l in zip(pred, labels) if x and l != 'SUPPORT')
    fn = sum(1 for x, l in zip(pred, labels) if not x and l == 'SUPPORT')
    rate = lambda lab: round(sum(1 for x, l in zip(pred, labels) if x and l == lab) / max(n[lab], 1), 4)
    return {
        'precision_supported': round(tp / max(tp + fp, 1), 4),
        'recall_supported': round(tp / max(tp + fn, 1), 4),
        'contradict_admitted': rate('CONTRADICT'),
        'nei_admitted': rate('NEI'),
    }


def grounding(path):
    rows, summary = load(path)
    labels = [r['label'] for r in rows]
    positive = [l == 'SUPPORT' for l in labels]
    p = [r['p'] if r['p'] is not None else 0.0 for r in rows]
    num = [bool(r['numbers_ok']) for r in rows]
    lex = [bool(r['lexical']) for r in rows]
    num_p = [x if ok else 0.0 for x, ok in zip(p, num)]
    report = {
        'suite': 'jev-grounding',
        'source': os.path.basename(path),
        'pairs': len(rows),
        'labels': dict(collections.Counter(labels)),
        'missing': sum(1 for r in rows if r['p'] is None),
        'summary': {k: v for k, v in summary.items() if k != 'summary'},
        'auc': {'judge': auc(p, positive), 'num+judge': auc(num_p, positive), 'lexical': auc([float(x) for x in lex], positive)},
        'lexical': grounding_metrics(lex, labels),
        'numbers_ok_only': grounding_metrics(num, labels),
        'thresholds': {},
    }
    for t in GROUNDING_THRESHOLDS:
        report['thresholds'][str(t)] = {
            'judge': grounding_metrics([x >= t for x in p], labels),
            'num+judge': grounding_metrics([x >= t for x in num_p], labels),
        }
    print('pairs', report['pairs'], 'missing', report['missing'], 'labels', report['labels'])
    print('auc', report['auc'])
    print('lexical', report['lexical'])
    for t, m in report['thresholds'].items():
        print(f'p>={t}', 'judge', m['judge'], '| num+judge', m['num+judge'])
    return report


def pairs(path):
    rows, summary = load(path)
    report = {'suite': 'jev-pairs', 'source': os.path.basename(path), 'items': len(rows),
              'missing': sum(1 for r in rows if r.get(SIGNALS[0]) is None),
              'summary': {k: v for k, v in summary.items() if k != 'summary'}, 'signals': {}}
    for sig in SIGNALS:
        scored = [(r[sig] if r.get(sig) is not None else 0.0, bool(r['gold'][sig]))
                  for r in rows if isinstance(r.get('gold'), dict) and sig in r['gold']]
        if not scored:
            continue
        s, g = [x for x, _ in scored], [y for _, y in scored]
        table = {}
        for t in PAIR_THRESHOLDS:
            tp = sum(1 for x, y in scored if x >= t and y)
            fp = sum(1 for x, y in scored if x >= t and not y)
            fn = sum(1 for x, y in scored if x < t and y)
            neg = sum(1 for y in g if not y)
            table[str(t)] = {'precision': round(tp / max(tp + fp, 1), 4), 'recall': round(tp / max(tp + fn, 1), 4),
                             'false_positive_rate': round(fp / max(neg, 1), 4)}
        report['signals'][sig] = {'n': len(scored), 'positives': sum(g), 'auc': auc(s, g), 'thresholds': table}
        print(sig, 'n', len(scored), 'auc', report['signals'][sig]['auc'])
    return report


if __name__ == '__main__':
    args = sys.argv[1:]
    save = None
    if '--save' in args:
        i = args.index('--save')
        save = args[i + 1]
        args = args[:i] + args[i + 2:]
    if len(args) != 2 or args[0] not in ('grounding', 'pairs'):
        sys.exit(__doc__)
    report = grounding(args[1]) if args[0] == 'grounding' else pairs(args[1])
    if save:
        json.dump(report, open(save, 'w'), indent=2)
        print('saved', save)
