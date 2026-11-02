# GitHub webhook fixtures

Recorded-shape GitHub App webhook payloads for `steve-github` tests. Each file starts from an
official example in [octokit/webhooks](https://github.com/octokit/webhooks) (`payload-examples/api.github.com/`, branch `main`)
and keeps its fields, nesting and value types. Only the values were swapped for the test values listed below.

Example base URL: `https://github.com/octokit/webhooks/blob/main/payload-examples/api.github.com/`
Docs base URL: `https://docs.github.com/en/webhooks/webhook-events-and-payloads`

| Fixture | `X-GitHub-Event` | `action` | octokit example | Docs |
|---|---|---|---|---|
| `ping.json` | `ping` | (none) | `ping/with-app_id.payload.json` | [#ping](https://docs.github.com/en/webhooks/webhook-events-and-payloads#ping) |
| `installation.created.json` | `installation` | `created` | `installation/created.payload.json` | [#installation](https://docs.github.com/en/webhooks/webhook-events-and-payloads#installation) |
| `installation_repositories.removed.json` | `installation_repositories` | `removed` | `installation_repositories/removed.payload.json` | [#installation_repositories](https://docs.github.com/en/webhooks/webhook-events-and-payloads#installation_repositories) |
| `push.json` | `push` | (none) | `push/with-new-branch.payload.json` | [#push](https://docs.github.com/en/webhooks/webhook-events-and-payloads#push) |
| `pull_request.opened.json` | `pull_request` | `opened` | `pull_request/opened.with-organization.payload.json` | [#pull_request](https://docs.github.com/en/webhooks/webhook-events-and-payloads#pull_request) |
| `pull_request.synchronize.json` | `pull_request` | `synchronize` | `pull_request/synchronize.payload.json` | [#pull_request](https://docs.github.com/en/webhooks/webhook-events-and-payloads#pull_request) |
| `pull_request.closed.merged.json` | `pull_request` | `closed` (`merged: true`) | `pull_request/closed.with-organization.payload.json` | [#pull_request](https://docs.github.com/en/webhooks/webhook-events-and-payloads#pull_request) |
| `pull_request.review_requested.json` | `pull_request` | `review_requested` | `pull_request/review_requested.payload.json` | [#pull_request](https://docs.github.com/en/webhooks/webhook-events-and-payloads#pull_request) |
| `pull_request_review.submitted.json` | `pull_request_review` | `submitted` | `pull_request_review/submitted.with-organization.payload.json` | [#pull_request_review](https://docs.github.com/en/webhooks/webhook-events-and-payloads#pull_request_review) |
| `check_suite.completed.success.json` | `check_suite` | `completed` | `check_suite/completed.1.payload.json` | [#check_suite](https://docs.github.com/en/webhooks/webhook-events-and-payloads#check_suite) |
| `check_suite.completed.failure.json` | `check_suite` | `completed` | `check_suite/completed.1.payload.json` (conclusion changed) | [#check_suite](https://docs.github.com/en/webhooks/webhook-events-and-payloads#check_suite) |
| `check_run.completed.success.json` | `check_run` | `completed` | `check_run/completed.1.payload.json` (conclusion changed) | [#check_run](https://docs.github.com/en/webhooks/webhook-events-and-payloads#check_run) |
| `check_run.completed.failure.json` | `check_run` | `completed` | `check_run/completed.1.payload.json` | [#check_run](https://docs.github.com/en/webhooks/webhook-events-and-payloads#check_run) |
| `workflow_run.completed.failure.json` | `workflow_run` | `completed` | `workflow_run/completed.with-pull-requests.payload.json` (conclusion changed) | [#workflow_run](https://docs.github.com/en/webhooks/webhook-events-and-payloads#workflow_run) |
| `issues.opened.json` | `issues` | `opened` | `issues/opened.with-organization.payload.json` | [#issues](https://docs.github.com/en/webhooks/webhook-events-and-payloads#issues) |
| `issue_comment.created.json` | `issue_comment` | `created` | `issue_comment/created.with-installation.payload.json` | [#issue_comment](https://docs.github.com/en/webhooks/webhook-events-and-payloads#issue_comment) |

## Substituted test values

- App: id `1001`, slug `steve-test`; installation id `4242` on organization `steve-test` (id `5001`).
- Repository: `steve-test/demo` (id `7001`, private, default branch `main`); all URLs rewritten to it.
- Users: `lee-test` (id `6001`, User) is the sender and human actor; `steve-test[bot]` (id `6002`, Bot) authors PR 12.
- PR 12 (id `9012`): head `steve/st-12` @ `a1b2c3d4…5678`, base `main` @ `0f1e2d3c…2d3c`.
  synchronize moves head to `b2c3d4e5…7890`; the merge commit is `c3d4e5f6…9012`. Open PRs carry test-merge sha `d4e5f607…1234`.
- Check suite `3001` and check run `9001` (`build`) belong to the GitHub Actions app (id `15368`, real public metadata kept, including its timestamps). Workflow run `8001` (`CI`), workflow id `2101`.
- Issue 13 (id `9013`). The PR's issue object uses id `9112`, comment `9101`, review `9201`, hook `4001`.
- Node IDs are base64 legacy IDs for the new numeric ids. All other timestamps fall on 2026-10-31, in the same order as in the source example. Fields that were unix ints in the example stay ints.

## Deviations from the source examples

- Repo-scoped events now carry `installation: {id, node_id}` and `organization` where the example did not. The `organization` shape is copied from `pull_request/closed.with-organization.payload.json`.
- `installation.app_slug` was added. It is in the octokit `common/installation.schema.json`.
- `ping`: `hook.type` is `App`. `repository` and `hook.test_url` were removed, and the hook URLs point at `/app/hook/...`, because an App hook is not repo-scoped. `ping_url` (`/app/hook/pings`) is kept because the octokit schema requires it, but it is not a documented REST endpoint.
- `issue_comment`: an `issue.pull_request` object (`url`, `html_url`, `diff_url`, `patch_url`, `merged_at`, as in the octokit issue schema) was added, and `issue.html_url` points to `/pull/12`.
- `push` is based on the new-branch example because the other push examples have no commits.
- Failure/success variants with no exact example reuse the sibling example. Only `status`/`conclusion` were changed.
- The embedded `check_run.check_suite` keeps the example's `status: "in_progress"` / `conclusion: null`.
