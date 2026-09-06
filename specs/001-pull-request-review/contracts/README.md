# Contracts: Pull Request Review

**Plan**: [../plan.md](../plan.md) | **Date**: 2026-09-06

This feature exposes no network or public library API. Its contracts are the four seams where it meets
something it does not own, and each is documented because a change on either side breaks the feature
silently otherwise.

| Contract | What it pins | Mandated by |
|---|---|---|
| [pull-request-host.md](./pull-request-host.md) | The single boundary for every host interaction, so a second platform or mechanism needs no change above it | spec FR-056, FR-057, FR-060, FR-061 |
| [changeset.md](./changeset.md) | The single boundary supplying the change under review, so branch comparison can be added later | spec FR-058, FR-059, FR-061 |
| [twg-cli.md](./twg-cli.md) | The observed command and JSON contract of the one shipped host implementation, including its three capability gaps | spec FR-062, FR-062a, FR-062b, FR-064 |
| [zed-surface.md](./zed-surface.md) | Every pre-existing file the feature modifies, and why each change is strictly additive | spec FR-074 – FR-082; constitution Principle VI |

## The rule that ties them together

The two boundary contracts exist because of a specific constraint. The constitution's Principle I rejects
speculative generality — traits-as-extension-points are refused by default — with **one** exception: a
seam a ratified specification mandates *by number*, naming the future source it exists to admit.

So both traits cite their requirement where they are declared, and both name what comes next: GitHub for
the host boundary, branch comparison for the changeset boundary. Neither has a registry, dynamic
discovery, or a plugin mechanism (FR-060); implementations are chosen where the caller is constructed.

`zed-surface.md` is the contract most likely to be consulted during an upstream merge, because it is the
list of places a conflict can occur at all.
</content>
