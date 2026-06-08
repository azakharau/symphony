argv = System.argv()

explicit_opencode_live? =
  Enum.any?(argv, &(&1 in ["--include=opencode_live", "--include=opencode_live:true", "--only=opencode_live", "--only=opencode_live:true"])) or
    Enum.any?(Enum.chunk_every(argv, 2, 1, :discard), fn [flag, filter] ->
      flag in ["--include", "--only"] and filter in ["opencode_live", "opencode_live:true"]
    end)

exclude = if explicit_opencode_live?, do: [], else: [opencode_live: true]

ExUnit.start(exclude: exclude, fail_if_no_tests: false)
Code.require_file("support/snapshot_support.exs", __DIR__)
Code.require_file("support/test_support.exs", __DIR__)
Code.require_file("support/isolation_regression_support.exs", __DIR__)
