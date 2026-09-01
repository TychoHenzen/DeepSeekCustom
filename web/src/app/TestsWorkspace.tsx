import { useMemo, useState } from 'react';

import type { AppCommand, AppCommandResult, RetainedTestResult, TestControlSnapshot, TestIdentity, TestScope } from '../client/contracts.ts';

interface Props { state: TestControlSnapshot; send: (command: AppCommand) => Promise<AppCommandResult> }

export function TestsWorkspace({ state, send }: Props) {
  const [filter, setFilter] = useState('');
  const [pending, setPending] = useState(false);
  const catalogue = state.discovery.catalogue;
  const query = filter.trim().toLocaleLowerCase();
  const modules = useMemo(() => catalogue?.modules.map((module) => ({
    ...module,
    tests: module.tests.filter((test) => test.name.toLocaleLowerCase().includes(query)),
  })).filter((module) => module.name.toLocaleLowerCase().includes(query) || module.tests.length > 0) ?? [], [catalogue, query]);

  async function command(value: AppCommand): Promise<void> {
    setPending(true);
    try { await send(value); } finally { setPending(false); }
  }
  function run(identity: TestIdentity): void {
    if (catalogue !== null) void command({ command: 'start_test_run', payload: { request: { identity, catalogue_revision: catalogue.discovered_at_ms } } });
  }
  const disabled = pending || state.active !== null || catalogue === null || state.discovery.catalogue_stale;

  return <section aria-labelledby="tests-title" className="tests-workspace">
    <header className="tests-toolbar"><div><h3 id="tests-title">Repository tests</h3><p>Run one approved Cargo test scope from the fixed project root.</p></div><button disabled={pending || state.active !== null} onClick={() => void command({ command: 'refresh_tests' })} type="button">Refresh catalogue</button></header>
    <DiscoveryFailure failure={state.discovery.failure} stale={state.discovery.catalogue_stale} />
    {catalogue === null ? <p role="status">Refresh the catalogue to select tests.</p> : <section aria-labelledby="catalogue-title" className="test-catalogue">
      <div className="test-catalogue-heading"><h4 id="catalogue-title">Test catalogue</h4><label htmlFor="test-filter">Filter modules and tests</label><input id="test-filter" onChange={(event) => setFilter(event.target.value)} type="search" value={filter} /></div>
      <button disabled={disabled} onClick={() => run(catalogue.full_workspace)} type="button">Run full workspace</button>
      <ol className="test-module-list">{modules.map((module) => <li key={module.name}><article className="test-module"><header><h5>{module.name}</h5><button disabled={disabled} onClick={() => run({ name: module.name, scope: { type: 'module', module: module.name } })} type="button">Run module</button></header><ul>{module.tests.map((test) => <li key={test.name}><code>{test.name}</code><button disabled={disabled} onClick={() => run(test)} type="button">Run exact test</button></li>)}</ul></article></li>)}</ol>
      {modules.length === 0 && <p>No catalogue entries match this filter.</p>}
    </section>}
    {state.active !== null && <ActiveRun run={state.active} cancel={() => void command({ command: 'cancel_test_run' })} pending={pending} />}
    <ResultHistory results={state.retained_results} warnings={state.retained_result_warnings} />
  </section>;
}

function DiscoveryFailure({ failure, stale }: { failure: TestControlSnapshot['discovery']['failure']; stale: boolean }) {
  if (failure === null) return null;
  return <section aria-labelledby="discovery-failure-title" className="test-failure" role="alert"><h4 id="discovery-failure-title">Catalogue refresh failed</h4><p>{stale ? 'The last successful catalogue remains visible but cannot start a run.' : 'No test catalogue is available.'}</p><p>Exit code: {failure.exit_code ?? 'process did not exit normally'}</p><pre><code>{failure.command.join(' ')}</code></pre><pre>{failure.diagnostic_output || 'No diagnostic output was captured.'}</pre></section>;
}

function ActiveRun({ run, cancel, pending }: { run: NonNullable<TestControlSnapshot['active']>; cancel: () => void; pending: boolean }) {
  return <section aria-labelledby="active-test-title" className="active-test-run"><h4 id="active-test-title">Active test run</h4><p><strong>{scopeLabel(run.identity.scope)}:</strong> {run.identity.name}</p><p aria-live="polite" role="status">Running for {run.elapsed_ms} ms. {run.counts.passed} passed, {run.counts.failed} failed.</p><button disabled={pending} onClick={cancel} type="button">Cancel test run</button><Output output={run.output} omitted={run.omitted_output_bytes} /></section>;
}

function ResultHistory({ results, warnings }: { results: RetainedTestResult[]; warnings: string[] }) {
  return <section aria-labelledby="test-results-title" className="test-results"><h3 id="test-results-title">Recent test results</h3>{warnings.map((warning) => <p key={warning} role="alert">{warning}</p>)}{results.length === 0 ? <p>No terminal test runs are retained.</p> : <ol aria-label="Newest test results first">{results.map((result) => <li key={result.run_id}><article className="test-result"><header><h4>{result.identity.name}</h4><p><strong>{resultLabel(result)}</strong></p></header><p>Scope: {scopeLabel(result.identity.scope)}</p><dl><div><dt>Started</dt><dd><time dateTime={new Date(result.started_at_ms).toISOString()}>{new Date(result.started_at_ms).toLocaleString()}</time></dd></div><div><dt>Duration</dt><dd>{result.duration_ms} ms</dd></div><div><dt>Passed</dt><dd>{result.counts.passed}</dd></div><div><dt>Failed</dt><dd>{result.counts.failed}</dd></div><div><dt>Ignored</dt><dd>{result.counts.ignored}</dd></div><div><dt>Filtered</dt><dd>{result.counts.filtered}</dd></div></dl>{result.failed_tests.length > 0 && <p role="alert">Failed tests: {result.failed_tests.join(', ')}</p>}<details><summary>Inspect command and diagnostic output</summary><h5>Command</h5><pre><code>{result.command.join(' ')}</code></pre><Output output={result.output} omitted={result.omitted_output_bytes} /></details></article></li>)}</ol>}</section>;
}

function Output({ output, omitted }: { output: string; omitted: number }) {
  return <div className="test-output"><h5>Output</h5>{omitted > 0 && <p role="status">Output was truncated. {omitted} bytes are omitted. The beginning and end are preserved.</p>}<pre tabIndex={0}>{output || 'No process output was captured.'}</pre></div>;
}

function scopeLabel(scope: TestScope): string {
  switch (scope.type) { case 'full_workspace': return 'Full workspace suite'; case 'module': return `Module ${scope.module}`; case 'exact': return `Exact test ${scope.test}`; }
}

function resultLabel(result: RetainedTestResult): string {
  const scope = scopeLabel(result.identity.scope);
  switch (result.outcome) { case 'passed': return `${scope} passed`; case 'failed': return `${scope} failed`; case 'cancelled': return `${scope} cancelled`; case 'infrastructure_error': return `${scope} infrastructure error`; }
}
