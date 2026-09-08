# Working folder selection

The Settings workspace shows the current agent working directory. Select
`Choose folder` to open the operating-system folder dialog.

The dialog runs through the Rust folder-picker port. The web application never
receives unrestricted filesystem access. A confirmed directory changes only
`working_dir`. A cancelled dialog leaves the prior value unchanged.

`project_root` remains fixed for `settings.json`, saved sessions, instructions,
Autopilot policy, and Procedure reports. Agent filesystem tools and backend CLI
children read the current `working_dir` for each operation.

The deterministic browser harness substitutes the folder dialog. It covers a
confirmed selection and cancellation without opening a real operating-system
window. Manual testing of the real dialog needs a desktop session.
