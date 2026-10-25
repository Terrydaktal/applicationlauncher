#!/usr/bin/env bash

# Sourced by the launch wrapper. Only a confirmed on-disk mismatch may build;
# changed source and stale running processes have separate recovery paths.
: "${binary:?}" "${daemon_binary:?}" "${state_dir:?}" "${repo_dir:?}" "${build_log:?}"

release_pair_identity() {
	stat -Lc '%d:%i:%s:%y:%z' -- "$binary" "$daemon_binary"
}

cached_binary_build_id() {
	local executable="$1" cache="$2"
	local identity cached_identity cached_build_id build_id temporary
	identity="$(stat -Lc '%d:%i:%s:%y:%z' -- "$executable")" || return 2
	if [[ -f "$cache" ]]; then
		{
			IFS= read -r cached_identity || true
			IFS= read -r cached_build_id || true
		} <"$cache"
		if [[ "${cached_identity:-}" == "$identity" && "${cached_build_id:-}" =~ ^[[:alnum:]_.+-]{1,256}$ ]]; then
			printf '%s\n' "$cached_build_id"
			return 0
		fi
	fi

	build_id="$(timeout --kill-after=1s 2s "$executable" --build-id)" || return 2
	[[ "$build_id" =~ ^[[:alnum:]_.+-]{1,256}$ ]] || return 2
	temporary="$(mktemp "$cache.XXXXXX")" || return 2
	if ! printf '%s\n%s\n' "$identity" "$build_id" >"$temporary" || ! mv -f -- "$temporary" "$cache"; then
		rm -f -- "$temporary"
		return 2
	fi
	printf '%s\n' "$build_id"
}

# Returns 0 for compatible, 1 for confirmed incompatible, 2 for unknown/racing.
release_pair_is_compatible() {
	local after
	pair_identity="$(release_pair_identity)" || return 2
	launcher_build_id="$(cached_binary_build_id "$binary" "$state_dir/launcher-build-id.cache")" || return 2
	daemon_build_id="$(cached_binary_build_id "$daemon_binary" "$state_dir/daemon-build-id.cache")" || return 2
	after="$(release_pair_identity)" || return 2
	[[ "$pair_identity" == "$after" ]] || return 2
	[[ "$launcher_build_id" == "$daemon_build_id" ]]
}

repair_retry_is_due() {
	local failed_at failed_identity now
	[[ -f "$state_dir/launcher-repair-failed" ]] || return 0
	IFS= read -r failed_at <"$state_dir/launcher-repair-failed" || return 0
	[[ "$failed_at" =~ ^[0-9]{1,12}$ ]] || return 0
	failed_identity="$(tail -n +2 "$state_dir/launcher-repair-failed")"
	[[ "$failed_identity" == "$pair_identity" ]] || return 0
	now="$(date +%s)"
	((now < failed_at || now - failed_at >= 300))
}

remember_repair_failure() {
	local temporary
	temporary="$(mktemp "$state_dir/launcher-repair-failed.XXXXXX")" || return 1
	{
		date +%s
		release_pair_identity || true
	} >"$temporary"
	mv -f -- "$temporary" "$state_dir/launcher-repair-failed"
}

# The wrapper owns the deployment flock for this entire bounded operation.
# Preserve live processes on failure; activation happens only after validation.
repair_release_pair() {
	local status
	if release_pair_is_compatible; then
		rm -f -- "$state_dir/launcher-repair-failed"
		return 0
	else
		status=$?
	fi
	[[ "$status" == 1 ]] || return 1
	if ! (
		cd -- "$repo_dir" || exit 1
		timeout --kill-after=5s 15m cargo build --manifest-path "$repo_dir/Cargo.toml" \
			--release --locked --bins --target-dir "$repo_dir/target"
	) 2>&1 | tail -c 262144 >"$build_log"; then
		remember_repair_failure
		return 1
	fi
	if ! release_pair_is_compatible; then
		printf '%s\n' 'Cargo completed without producing a compatible launcher/daemon pair.' >>"$build_log"
		remember_repair_failure
		return 1
	fi
	rm -f -- "$state_dir/launcher-repair-failed"
}
