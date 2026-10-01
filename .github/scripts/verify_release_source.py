#!/usr/bin/env python3
"""Bind release assets to the commit named by their version tag."""

import argparse
import re
import subprocess


def git(*arguments):
    return subprocess.run(
        ["git", *arguments], check=True, text=True, capture_output=True
    ).stdout.strip()


def verify(tag, commit, *, require_tag=False, create_tag=False):
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag):
        raise ValueError("release tag must be vX.Y.Z")
    if not re.fullmatch(r"[0-9a-f]{40,64}", commit):
        raise ValueError("release commit must be a complete object id")
    if git("rev-parse", "HEAD^{commit}") != commit:
        raise ValueError("checked-out commit does not match the release source")

    reference = f"refs/tags/{tag}"
    # A successful empty lookup means absence. Transport failures must abort.
    remote = git("ls-remote", "--refs", "origin", reference)
    if not remote:
        if require_tag:
            raise ValueError(f"rebuild requires the existing tag {tag}")
        if create_tag:
            # Without force, the server creates the tag only if it is absent.
            # A competing tag creation fails before any assets are uploaded.
            git("push", "origin", f"{commit}:{reference}")
        return

    git("fetch", "--no-tags", "origin", reference)
    tagged_commit = git("rev-parse", "FETCH_HEAD^{commit}")
    if tagged_commit != commit:
        raise ValueError(
            f"{tag} names {tagged_commit}, but this run uses {commit}. "
            f"Run the rebuild from {tag}."
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--require-tag", action="store_true")
    parser.add_argument("--create-tag", action="store_true")
    args = parser.parse_args()
    try:
        verify(
            args.tag,
            args.commit,
            require_tag=args.require_tag,
            create_tag=args.create_tag,
        )
    except (ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"release source verification failed: {error}\n")


if __name__ == "__main__":
    main()
