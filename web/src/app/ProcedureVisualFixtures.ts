import type { OperationState } from '../client/contracts.ts';

export interface ProcedureVisualFixture {
  name: string;
  report: OperationState;
}

const report = (operationId: string, phase: OperationState['phase'], message: string, error: OperationState['error'] = null): OperationState => ({
  kind: 'procedure',
  operation_id: operationId,
  phase,
  progress: { completed: phase === 'running' ? 2 : 4, total: 4 },
  message,
  error,
});

/** Deterministic reports used to maintain every Procedure presentation state. */
export const procedureVisualFixtures: ProcedureVisualFixture[] = [
  {
    name: 'running-local-route',
    report: report('procedure-running', 'running', 'stage: localization\nroute: local\ntarget: crates/deepseek-custom/src/application/dto.rs\nevidence: symbol matched'),
  },
  {
    name: 'awaiting-review',
    report: report('procedure-review', 'awaiting_review', 'stage: review\nroute: frontier\ntarget: web/src/app/OperationWorkspace.tsx\nevidence: verifier passed\ndiff: @@ -12 +12 @@\nreport: .deepseek/procedure-runs/procedure-review.json'),
  },
  {
    name: 'succeeded',
    report: report('procedure-success', 'completed', 'outcome: succeeded\nroute: local\nreport: .deepseek/procedure-runs/procedure-success.json'),
  },
  {
    name: 'failed',
    report: report('procedure-failed', 'failed', 'outcome: failed\nroute: frontier\nevidence: verifier rejected candidate', { code: 'service_failed', message: 'Verifier command exited with code 1.', recoverable: true, field: null }),
  },
  {
    name: 'interrupted',
    report: report('procedure-interrupted', 'interrupted', 'outcome: interrupted\nstage: patch preview\nreport: .deepseek/procedure-runs/procedure-interrupted.json'),
  },
];
