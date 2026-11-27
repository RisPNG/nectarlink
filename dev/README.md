# Fork development policy

This policy is required for everyone working on the RisPNG NectarLink fork, including automated contributors. Read it before starting work and follow it when creating, updating, and merging branches. If this file is absent from the checked-out branch, run `mise exec -- git show origin/canary:dev/README.md` to read it from the fork's integration branch.

## Branches

| Ref | Role |
| --- | --- |
| `upstream/main` | The original NectarLink project's main branch and the base for new contributions. |
| `origin/main` | The fork's mirror of `upstream/main`. Update it only by fast-forwarding to upstream. |
| `origin/canary` | The fork's default branch. It contains upstream updates, all active contributions, and fork maintenance. |

When you clone the fork, `origin` is `RisPNG/nectarlink`. Add the original `thtbee/nectarlink` repository as `upstream` if the remote is missing.

```sh
mise exec -- git remote add upstream https://github.com/thtbee/nectarlink.git
```

## Prepare a contribution

1. Fetch `upstream` and start a separate branch from the current `upstream/main` for each feature or fix. Use a descriptive `feature/<topic>` or `fix/<topic>` name.
2. Make the branch a complete change that upstream can review and merge on its own. Follow NectarLink's architecture and conventions, and include the code, tests, and documentation needed for that change.
3. Keep integration merges and fork maintenance out of contribution branches. Review the branch against its upstream base, run the relevant checks, and record any verification that could not run.
4. Sign off contribution commits under the Developer Certificate of Origin. Follow the original project's requirements when preparing an upstream submission. [Upstream contribution guide](../CONTRIBUTING.md)

## Integrate and update

1. Push each validated contribution branch to `origin`. Merge it into `canary` with a merge commit so its history stays identifiable. Verify the combined changes and push `canary` to `origin`.
2. To bring in upstream changes, fetch `upstream` and merge `upstream/main` into `canary`. Resolve conflicts in the integration branch, verify the result, and push it to `origin`.
3. Keep fork maintenance, including this policy, in `canary`. Base any review branch for fork maintenance on `canary` and merge it back there.
