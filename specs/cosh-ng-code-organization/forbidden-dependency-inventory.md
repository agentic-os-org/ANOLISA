# cosh-shell Forbidden Dependency Inventory

The audit registers only the two test-only DTO references below. Production
shell_host code does not depend on approval rendering or runtime mutation.

| registered_hits | path | direction | owner | status | close condition |
| ---: | --- | --- | --- | --- | --- |
| 2 | `crates/cosh-shell/src/shell_host/raw_relay_tests.rs` | shell_host tests -> ui action DTO | raw_input | D22 stable contract | Remove when the ApprovalActionSet DTO owner changes. |

Any increase remains a layout failure. This entry permits no rendering,
approval decision, or agent/runtime side effect in shell_host.
