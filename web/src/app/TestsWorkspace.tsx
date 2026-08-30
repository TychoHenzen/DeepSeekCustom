import type { TestResultHistory } from '../client/contracts.ts';

export function TestsWorkspace({ history }: { history: TestResultHistory }) {
  return (
    <section aria-labelledby="test-results-title" className="test-results">
      <h3 id="test-results-title">Recent test results</h3>
      {history.retained_result_warnings.map((warning) => <p key={warning} role="alert">{warning}</p>)}
      {history.retained_results.length === 0 ? (
        <p>No completed test runs are retained.</p>
      ) : (
        <ol aria-label="Newest test results first">
          {history.retained_results.map((result) => (
            <li key={result.run_id}>
              <article className="test-result">
                <header>
                  <h4>{result.identity.name}</h4>
                  <p><strong>{result.outcome.replace('_', ' ')}</strong></p>
                </header>
                <dl>
                  <div><dt>Started</dt><dd><time dateTime={new Date(result.started_at_ms).toISOString()}>{new Date(result.started_at_ms).toLocaleString()}</time></dd></div>
                  <div><dt>Duration</dt><dd>{result.duration_ms} ms</dd></div>
                  <div><dt>Passed</dt><dd>{result.counts.passed}</dd></div>
                  <div><dt>Failed</dt><dd>{result.counts.failed}</dd></div>
                  <div><dt>Ignored</dt><dd>{result.counts.ignored}</dd></div>
                  <div><dt>Filtered</dt><dd>{result.counts.filtered}</dd></div>
                </dl>
                <details>
                  <summary>Inspect command and diagnostic output</summary>
                  <h5>Command</h5>
                  <pre><code>{result.command.join(' ')}</code></pre>
                  <h5>Output</h5>
                  <pre>{result.output || 'No process output was captured.'}</pre>
                </details>
              </article>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
