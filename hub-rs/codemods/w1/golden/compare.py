#!/usr/bin/env python3
"""Compare two recordings, optionally enforce the post-S1 workspace contract."""
import argparse
import json
from normalize import normalized

def comparable(path, scoped=False, project=False):
    steps, streams = normalized(path, scoped, project)
    for groups in streams.values():
        for frames in groups.values():
            for frame in frames:
                frame.pop('ws')  # Validated separately, never silently trusted for ownership.
    return steps, streams


def compare(before, after, scoped=False):
    left = comparable(before, project=scoped)
    right = comparable(after, scoped=scoped)
    if left != right:
        import difflib
        diff = '\n'.join(difflib.unified_diff(
            json.dumps(left, indent=2, sort_keys=True).splitlines(),
            json.dumps(right, indent=2, sort_keys=True).splitlines(),
            fromfile=str(before), tofile=str(after)))
        raise ValueError(diff)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('before')
    parser.add_argument('after')
    parser.add_argument('--scoped', action='store_true',
                        help='after is post-S1: enforce ws and exclude legacy cross-workspace broadcasts')
    args = parser.parse_args()
    try:
        compare(args.before, args.after, args.scoped)
    except (ValueError, KeyError) as error:
        parser.exit(1, str(error) + '\n')
    print(f'equal: {args.before} == {args.after}' + (' (scoped)' if args.scoped else ''))


if __name__ == '__main__':
    main()
