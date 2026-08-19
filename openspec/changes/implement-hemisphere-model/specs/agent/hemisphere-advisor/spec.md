## Purpose

Gives the primary agent a second opinion it cannot skip. A background advisor watches the conversation on its own backend, sees a compressed view of it rather than the whole history, and asks the primary agent one short question per turn when something looks wrong.

## ADDED Requirements

### Requirement: An advisor pass runs once per primary turn while the mode is on

While hemisphere mode is enabled, exactly one advisor pass SHALL run after each primary turn finishes, and never during one. While the mode is disabled, no advisor pass SHALL run and no advisor backend SHALL be built.

The pass runs after the turn rather than during it because the advisor's whole input is what the turn produced. A pass mid-turn would see a conversation the turn is still writing.

#### Scenario: One pass follows one turn

- **WHEN** hemisphere mode is enabled and the primary agent finishes a turn
- **THEN** exactly one advisor pass runs, and it starts after that turn has produced its final reply

#### Scenario: The mode off costs nothing

- **WHEN** hemisphere mode is disabled and the primary agent runs a turn
- **THEN** no advisor pass runs, no advisor backend is built, and no request reaches any advisor provider

#### Scenario: A turn that fails still ends the pass cycle

- **WHEN** a primary turn ends in an error or an interrupt rather than a normal reply
- **THEN** no advisor pass runs for that turn, and the next successful turn is followed by a pass as normal

### Requirement: The advisor runs on its own configured backend

The advisor's target SHALL be a named backend entry, configured independently of the backend the primary agent runs on. The advisor SHALL be able to run on a different provider and a different model from the primary agent.

Naming an entry rather than a bare model string is what lets the advisor be a cheap or local model while the primary agent runs on an expensive one. The cost of the feature is one extra call per turn, so the target must be separately choosable or the feature doubles the session's bill.

#### Scenario: The advisor runs on a different provider

- **WHEN** hemisphere mode is enabled, the primary agent runs on one configured backend, and the advisor names a different configured backend
- **THEN** the advisor's request goes to the provider that second entry names, and the primary agent's own backend and model are unchanged

#### Scenario: An unnamed or unknown advisor backend disables the pass

- **WHEN** hemisphere mode is enabled and the configured advisor backend names no entry that exists
- **THEN** no pass runs, the failure is reported once where the user can see it, naming both the requested entry and the entries that exist, and the primary agent's turn is unaffected

### Requirement: The advisor sees a compressed view, never the whole conversation

The advisor SHALL receive a compressed rendering of the conversation, bounded by a configured character or token budget. It SHALL NOT receive the primary agent's full message history.

The compressed rendering SHALL keep the most recent turns in readable form and SHALL summarize what came before them. Any summary of earlier turns SHALL describe only what those turns actually contain. It SHALL NOT assert that any particular activity happened.

Truncating a message for the compressed view SHALL cut on a character boundary, never inside a multi-byte character.

#### Scenario: The view stays inside its budget

- **WHEN** a conversation has grown past the advisor's configured view budget and a pass runs
- **THEN** the rendering handed to the advisor is within that budget, and it holds the most recent turns rather than the oldest

#### Scenario: A summary claims nothing it cannot see

- **WHEN** earlier turns are summarized rather than rendered in full
- **THEN** the summary states how much was elided and does not assert that any tool ran, any file was read, or any code was written

#### Scenario: A multi-byte message truncates safely

- **WHEN** a message long enough to need truncation holds multi-byte characters at the cut point
- **THEN** the rendering completes without panicking, and the truncated text ends on a whole character

### Requirement: An advisor question reaches the primary agent before its next turn

When an advisor pass produces a question, that question SHALL appear in the primary agent's conversation as a user-role message, prefixed so the primary agent can tell it apart from anything the human typed. The message SHALL be in place before the primary agent builds its next request.

At most one question per pass SHALL reach the primary agent. An advisor that returns several SHALL have its reply reduced to one, so a single pass cannot flood the primary agent's context.

#### Scenario: A question lands before the next turn

- **WHEN** an advisor pass returns a question and the user then sends another message
- **THEN** the primary agent's next request holds the advisor's question as a user-role message, positioned before the user's new message

#### Scenario: The question is attributable

- **WHEN** an advisor question is injected into the conversation
- **THEN** its text is prefixed to name the advisor as its source, so neither the primary agent nor a reader of the record mistakes it for something the human asked

#### Scenario: One question per pass at most

- **WHEN** an advisor pass returns a reply holding more than one question
- **THEN** exactly one question is injected into the primary agent's conversation

### Requirement: Silence is free

An advisor pass that has nothing useful to add SHALL inject nothing into the primary agent's conversation. Silence SHALL cost the primary agent no context tokens.

Silence is the expected common case. An advisor that must say something every turn would fill the primary agent's context with filler, which is worse than having no advisor.

#### Scenario: An empty reply injects nothing

- **WHEN** an advisor pass returns an empty reply, or a reply that holds no question
- **THEN** nothing is added to the primary agent's conversation, and its next request is byte-for-byte what it would have been with the mode off

#### Scenario: A silent pass is still visible

- **WHEN** an advisor pass runs and stays silent
- **THEN** the transcript still records that the pass ran, so a user can tell a silent advisor from an advisor that never ran

### Requirement: An advisor pass never breaks the primary turn

A failed, timed out, or interrupted advisor pass SHALL NOT fail the primary agent's turn, and SHALL NOT prevent the next turn from running. Every such failure SHALL be reported where the user can see it and SHALL leave the primary agent's conversation unchanged.

#### Scenario: A dead advisor backend leaves the session running

- **WHEN** an advisor pass fails because its provider is unreachable or returns an error
- **THEN** the failure is reported, the primary agent's conversation is unchanged, and the next primary turn runs normally

#### Scenario: An interrupt reaches a running pass

- **WHEN** the user interrupts while an advisor pass is in flight
- **THEN** that pass stops, nothing is injected into the primary agent's conversation, and the interrupt is not consumed in a way that leaves a later turn unstoppable

### Requirement: The advisor is visible and controllable from the interface

Hemisphere mode SHALL have a control in the settings interface that turns it on and off. That control SHALL persist its state, so the mode survives a restart.

An advisor pass SHALL appear in the transcript as its own collapsed block, showing which backend and model it ran on, in the same shape any other dispatched agent's work appears. Its block SHALL NOT expand on its own while it runs.

#### Scenario: The mode is reachable without editing a file

- **WHEN** a user opens the settings interface
- **THEN** a control for hemisphere mode is present, and toggling it changes whether a pass runs on the next turn

#### Scenario: The mode survives a restart

- **WHEN** a user enables hemisphere mode and restarts the application
- **THEN** the mode is still enabled, and the configured advisor backend is still the one that was set

#### Scenario: A pass draws as its own block

- **WHEN** an advisor pass runs
- **THEN** the transcript holds a collapsed block for it naming the backend and model it ran on, and that block does not expand by itself

### Requirement: Advisor state ends when the conversation does

The advisor SHALL carry no state that outlives the conversation it advises. Resetting the session, opening a new conversation, and starting a new autopilot iteration SHALL each leave the advisor with nothing carried over from before.

#### Scenario: A reset clears the advisor

- **WHEN** the conversation is reset while hemisphere mode is on
- **THEN** the next advisor pass sees only the new conversation, and no question from the old one reaches the primary agent

#### Scenario: An autopilot iteration starts clean

- **WHEN** an autopilot run starts a new iteration while hemisphere mode is on
- **THEN** that iteration's first advisor pass sees only that iteration's conversation

### Requirement: The mode applies to the in-process backend only

Hemisphere mode SHALL apply only to the backend whose turn loop this harness owns. A backend that runs its own turn loop in a child process SHALL be unaffected, whatever the mode is set to.

Enabling the mode while such a backend is active SHALL say so where the user can see it, rather than appearing to work.

#### Scenario: The child-process backend ignores the mode

- **WHEN** hemisphere mode is enabled and the active backend runs its turn loop in a child process
- **THEN** no advisor pass runs, and the interface says the mode does not apply to that backend

### Requirement: The pass is bounded in cost per turn

One primary turn SHALL produce at most one advisor request. The advisor's reply SHALL be capped in length by configuration, and that cap SHALL be low enough that a question costs a small fraction of a primary turn.

#### Scenario: One request per turn, whatever the turn did

- **WHEN** a primary turn runs many internal rounds and many tool calls before finishing
- **THEN** that turn is still followed by exactly one advisor request
