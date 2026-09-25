# SonarCloud OSS setup (dunnage)

Maintainer guide for the SonarCloud workflow in `.github/workflows/sonarcloud.yml` and the scanner
configuration in `sonar-project.properties`.

The `sonar.organization` / `sonar.projectKey` values (`listepo` / `listepo_dunnage`) are
**placeholders** until they match the SonarCloud UI after you import the project.
As of 2026-09-25 the public SonarCloud API reports no organization with the key
`listepo`, so the organization still has to be created (bound to the `listepo`
GitHub account) on first import.

## 1. Import the repository

1. Sign in at [sonarcloud.io](https://sonarcloud.io) with GitHub.
2. Create or pick the organization bound to the **listepo** GitHub account and choose
   the free plan for open-source (public) projects.
3. Import the **listepo/dunnage** repository.
4. Compare the **organization key** and **project key** shown in the UI with
   `sonar-project.properties`. If they differ, update the file so they match exactly;
   a mismatch makes the scan fail or report into the wrong project.

## 2. Turn Automatic Analysis off

This project is analyzed by CI (`SonarSource/sonarqube-scan-action`), so SonarCloud's
**Automatic Analysis** must be **off**. With both enabled, the CI scan fails with an
error about Automatic Analysis being enabled.

In the SonarCloud project: **Administration → Analysis Method → Automatic Analysis → off**.

## 3. Create a token

1. Open <https://sonarcloud.io/account/security>
   (**My Account → Security**, section **Access Tokens / Personal Tokens**).
2. Generate a token (for example `dunnage-github-actions`).
3. Copy the value; it is shown only once.

## 4. Add the `SONAR_TOKEN` secret

On **listepo/dunnage**: **Settings → Secrets and variables → Actions → New repository secret**

- Name: `SONAR_TOKEN`
- Value: the token from step 3

Never commit the token. Without the secret (fork pull requests, or before it is added)
every step in the workflow soft-skips with a notice, so the check stays green; a green
run in that state does **not** mean an analysis happened.

## 5. When the analysis runs

- On pull requests targeting `main` (draft pull requests are skipped until they are
  marked ready for review). Pull request decoration needs the SonarCloud GitHub App,
  which the import in step 1 installs.
- On every push to `main`.
- Manually via **Actions → sonarcloud → Run workflow**.

## 6. Soft-fail for now, blocking later

The workflow does not fail the build yet: the coverage and scan steps use
`continue-on-error: true`, and the Quality Gate is not awaited. To make it blocking
once the dashboard looks sane:

1. Remove `continue-on-error: true` from the scan (and, if wanted, coverage) steps.
2. Add `sonar.qualitygate.wait=true` to `sonar-project.properties` so the scan step
   fails when the Quality Gate fails.
3. Optionally mark the `sonarcloud` check as required in the branch protection rules
   for `main`.

## 7. Coverage

The workflow installs `llvm-tools-preview` and `cargo-llvm-cov`, runs

```bash
mkdir -p coverage
cargo llvm-cov --workspace --locked --lcov --output-path coverage/lcov.info
```

and points Sonar at the report with **`sonar.rust.lcov.reportPaths=coverage/lcov.info`**
(not `sonar.coverageReportPaths`). The step is best effort: if tests fail or the report
is missing, the scan still runs, just without coverage.

`scripts/` (shell), `docs/` (including the spike scripts) are outside the analysis scope.

## 8. Local dry run (optional)

```bash
mise install
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked   # or: brew install cargo-llvm-cov
mkdir -p coverage
cargo llvm-cov --workspace --locked --lcov --output-path coverage/lcov.info
```

Run the scanner locally only with `SONAR_TOKEN` exported in your shell; never write the
token into the repository.

## References

- Workflow: `.github/workflows/sonarcloud.yml`
- Scanner configuration: `sonar-project.properties`
- [SonarQube Cloud documentation](https://docs.sonarsource.com/sonarqube-cloud/)
