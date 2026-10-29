# Attack suite

Every attack below must fail. Each defense is proven by the tests named in
its row; scripts/attack-suite.sh runs the whole test suite and fails if any
named test is missing or does not pass, so this map cannot drift from the
code. CI runs the script on every change.

Test names are the only text in backticks in this file.

## Read the provider key

The key lives only in the gateway vault, encrypted, and is sent to the
provider only in an authorization header. No other process holds it, and
nothing returns, logs or audits it.

| Defense | Tests |
|---|---|
| The database holds only ciphertext; the key is never returned | `database_never_holds_the_plaintext`, `stored_key_is_never_returned`, `wrong_master_key_cannot_decrypt`, `ciphertext_moved_to_another_provider_fails` |
| The key is sent only in a header, never in text sent to the model | `sends_key_only_in_header`, `sends_key_only_in_authorization_header` |
| Provider errors never carry the key | `maps_errors_retries_overload_and_redacts_the_key`, `maps_provider_errors_without_leaking_the_key`, `clean_redacts_strips_and_truncates` |
| The engine refuses provider keys and gateway secrets | `rejects_provider_keys_by_name_and_value`, `the_engine_refuses_the_gateway_admin_secret` |
| The engine's database role cannot read credentials | `each_service_role_gets_only_what_its_code_uses` |
| The server passes keys through without keeping or returning them | `keys_go_to_the_gateway_with_the_admin_secret_and_never_come_back`, `the_server_refuses_the_issuer_secret_the_master_key_and_provider_keys` |
| Code in the sandbox sees no secrets, no Docker socket | `the_servers_environment_does_not_reach_the_code`, `the_docker_socket_and_host_secrets_are_out_of_reach` |
| Configuration and audit output never show secrets | `debug_output_does_not_reveal_secrets`, `changes_are_recorded_in_order_without_secrets_and_chained` |

## Spend more than allowed

| Defense | Tests |
|---|---|
| The highest possible cost is reserved before every call, under parallel load | `parallel_reservations_never_exceed_the_task_budget`, `call_that_cannot_fit_the_task_budget_is_rejected`, `task_stops_when_its_budget_runs_out` |
| Agent and workspace monthly limits | `agent_monthly_budget_is_enforced`, `agent_monthly_limit_applies_across_tasks`, `workspace_limit_applies_across_tasks` |
| Tokens cannot mint tokens; the admin secret cannot spend | `task_token_cannot_issue_more_tokens`, `each_secret_opens_only_its_own_routes` |
| A refused reservation pauses the run for a person; only administrators raise budgets | `pauses_when_the_budget_is_spent`, `budget_is_for_administrators_and_cancelling_needs_manage` |
| No costlier model without explicit permission | `never_moves_up_without_permission`, `stops_moving_up_at_the_configured_limit` |
| Sandbox code cannot exhaust the host | `a_fork_bomb_hits_the_process_limit`, `a_memory_bomb_is_stopped`, `an_endless_loop_is_stopped_and_its_container_removed`, `output_is_capped_without_blocking_the_program` |

## Leave the allowed folder

| Defense | Tests |
|---|---|
| Task ids cannot form paths | `task_ids_cannot_leave_the_root` |
| Only the task's own folder is visible, read-only; nothing else is writable | `only_the_tasks_own_folder_is_visible_and_nothing_can_be_written_outside_tmp` |
| Result files are plain files with simple names, never links, within limits | `result_files_come_back_within_limits_and_the_next_run_sees_them` |
| Code runs unprivileged | `code_runs_as_nobody_without_capabilities_or_new_privileges` |

## Reach addresses that are not allowed

| Defense | Tests |
|---|---|
| Sandbox code has no network | `there_is_no_network` |
| Provider calls are https only and never follow redirects | `provider_calls_never_follow_redirects` |
| Internal clients accept only https or loopback | `accepts_https_and_loopback_http_only`, `the_sandbox_must_be_on_loopback_or_https` |

## Act without permission, or be steered by injected text

| Defense | Tests |
|---|---|
| Injected instructions cannot run an irreversible action | `instructions_hidden_in_tool_results_cannot_run_an_irreversible_action`, `irreversible_tool_waits_for_approval`, `anything_but_explicit_approval_rejects` |
| An approved action runs at most once, even after a crash or a race | `an_action_started_before_a_crash_never_runs_again`, `concurrent_decisions_run_the_tool_once` |
| Tools outside the run's set cannot be called | `tools_outside_the_subset_cannot_be_called` |
| Only people allowed on the agent decide, once per request | `only_people_allowed_on_the_agent_decide_once_and_each_request_is_claimed_once` |

## Impersonate someone or steal a session

| Defense | Tests |
|---|---|
| Task tokens: unknown, expired, revoked and misused secrets are refused | `rejects_unknown_token`, `expired_token_is_rejected`, `revoked_token_stops_working`, `rejects_issuer_secret_used_as_task_token` |
| Sign-in resists guessing and does not reveal accounts | `ten_wrong_passwords_lock_the_account`, `one_address_gets_twenty_sign_in_attempts_per_window`, `a_wrong_password_and_an_unknown_account_get_the_same_answer` |
| Sessions are strict cookies that end when they should | `sign_in_sets_a_strict_host_cookie_and_sign_out_ends_the_session`, `sessions_end_when_idle_or_when_the_account_changes`, `disabling_an_account_ends_its_sessions_at_once` |
| Other sites cannot act for a signed-in person | `state_changes_from_other_origins_are_refused` |
| Administrative routes need an administrator; the sandbox needs its secret | `only_administrators_manage_accounts`, `only_administrators_reach_the_gateway`, `the_service_runs_code_only_with_the_secret` |

## Tamper with records

| Defense | Tests |
|---|---|
| The audit log is append-only and hash-chained | `the_log_cannot_be_changed_and_tampering_is_found` |
| Usage records cannot be changed or deleted | `usage_events_are_append_only` |
| Runs cannot be overwritten by a stale copy | `stale_saves_are_rejected` |
