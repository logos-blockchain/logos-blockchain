from __future__ import annotations

from sys import stderr
from contextlib import suppress
import time
from typing import Optional, Sequence


class CargoHackDashboard:
    _STATUS_WIDTH = len("SKIPPED (cache hit)")
    _RICH_CRATE_WIDTH_CAP = 48
    _SECONDS_WIDTH = 6

    def __init__(
        self,
        total_crates: int,
        tag: str,
        max_crate_name_width: int = 0,
        rich_enabled: Optional[bool] = None,
        verbose: bool = False,
    ):
        self.total_crates = total_crates
        self.tag = tag
        self.max_crate_name_width = max_crate_name_width
        self.verbose = verbose
        self._rich_crate_width = min(max(self.max_crate_name_width, 24), self._RICH_CRATE_WIDTH_CAP)

        self.success = 0
        self.skipped = 0
        self.failed = 0
        self.cache_hits = 0

        self._start_time = time.monotonic()

        self._rich_enabled = self._should_use_rich(rich_enabled)

        self._progress = None
        self._summary = None
        self._live = None
        self._workspace_task: Optional[int] = None
        self._summary_task: Optional[int] = None
        self._console = None
        self._active_crate: Optional[tuple[str, int]] = None

        if self._rich_enabled:
            self._init_rich()

    # ----------------------------------------------------------

    def elapsed_seconds(self) -> float:
        return time.monotonic() - self._start_time

    # ----------------------------------------------------------

    def _format_results(self) -> str:
        return f"ok={self.success} skip={self.skipped} (cache={self.cache_hits}) fail={self.failed}"

    # ----------------------------------------------------------

    def _format_position(self, index: int) -> str:
        width = len(str(self.total_crates))
        return f"[{index:>{width}}/{self.total_crates}]"

    # ----------------------------------------------------------

    def _format_status(self, status: str) -> str:
        return f"{status:<{self._STATUS_WIDTH}}"

    # ----------------------------------------------------------

    def _format_crate_name(self, crate_name: str) -> str:
        width = max(self.max_crate_name_width, len(crate_name))
        return f"{crate_name:<{width}}"

    # ----------------------------------------------------------

    def _format_rich_crate_name(self, crate_name: str) -> str:
        if len(crate_name) > self._rich_crate_width:
            return f"{crate_name[:self._rich_crate_width - 3]}..."
        return f"{crate_name:<{self._rich_crate_width}}"

    # ----------------------------------------------------------

    def _format_seconds(self, seconds: float) -> str:
        return f"{seconds:{self._SECONDS_WIDTH}.1f}s"

    # ----------------------------------------------------------

    def _should_use_rich(self, rich_enabled: Optional[bool]) -> bool:
        if rich_enabled is not None:
            return rich_enabled

        if not stderr.isatty():
            return False

        with suppress(Exception):
            from rich.console import Console
            return Console(file=stderr).is_terminal

        return False

    # ----------------------------------------------------------

    def _init_rich(self):
        from rich.console import Console
        from rich.live import Live
        from rich.progress import (
            Progress,
            SpinnerColumn,
            BarColumn,
            TextColumn,
            TimeElapsedColumn,
        )
        from rich.console import Group

        self._console = Console(
            file=stderr,
            force_terminal=True,
            force_interactive=True,
        )

        self._progress = Progress(
            SpinnerColumn(),
            BarColumn(),
            TextColumn("{task.completed}/{task.total}"),
            TimeElapsedColumn(),
            TextColumn("|"),
            TextColumn("{task.description}"),
            console=self._console,
            refresh_per_second=12,
        )
        self._summary = Progress(
            TextColumn("{task.description}"),
            console=self._console,
            refresh_per_second=12,
        )

        self._live = Live(
            Group(self._progress, self._summary),
            console=self._console,
            refresh_per_second=12,
        )
        self._live.start()

        self._workspace_task = self._progress.add_task(
            "",
            total=self.total_crates,
        )
        self._summary_task = self._summary.add_task("")

    # ----------------------------------------------------------

    def log(self, message: str):
        if self._rich_enabled:
            self._console.print(message)
        else:
            print(message, file=stderr)

    # ----------------------------------------------------------

    def log_crate_detail(self, crate_name: str, message: str):
        if self._rich_enabled:
            return

        if self._active_crate and self._active_crate[0] == crate_name:
            _, index = self._active_crate
            self.log(f"{self.tag} {self._format_position(index)} {message}")
            return

        self.log(f"{self.tag} ({crate_name}) {message}")

    # ----------------------------------------------------------

    def log_cache_invalidation_summary(self, removed: int, missing: int):
        parts = [f"{removed} removed", f"{missing} already missing"]
        self.log(f"    - dependent cache invalidation: {', '.join(parts)}.")

    # ----------------------------------------------------------

    def close(self):
        if self._rich_enabled and self._live:
            self._live.stop()

    # ----------------------------------------------------------

    def start_crate(self, crate_name: str, index: int):
        self._active_crate = (crate_name, index)

        if not self._rich_enabled:
            print(
                f"{self.tag} {self._format_position(index)} Checking {crate_name}",
                file=stderr,
            )
            return

        self._progress.update(
            self._workspace_task,
            description=f"Checking: {self._format_rich_crate_name(crate_name)}",
        )
        self._summary.update(
            self._summary_task,
            description=f"Progress: {self._format_results()}",
        )

    # ----------------------------------------------------------

    def finish_crate(
        self,
        *,
        crate_name: str,
        index: int,
        skipped: bool,
        success: bool,
        crate_elapsed: float,
    ):
        status = "OK"
        if skipped:
            self.skipped += 1
            self.cache_hits += 1
            status = "SKIPPED (cache hit)"
        elif success:
            self.success += 1
            status = "OK"
        else:
            self.failed += 1
            status = "FAILED"

        total_elapsed = self.elapsed_seconds()
        if not self._rich_enabled:
            print(
                f"{self.tag} {self._format_position(index)} {self._format_status(status)} "
                f"(crate {self._format_seconds(crate_elapsed)}, total {self._format_seconds(total_elapsed)})",
                file=stderr,
            )
            self._active_crate = None
            return

        message = (
            f"{self.tag} {self._format_position(index)} {self._format_crate_name(crate_name)}: "
            f"{self._format_status(status)} "
            f"(crate {self._format_seconds(crate_elapsed)}, total {self._format_seconds(total_elapsed)})"
        )

        self._progress.advance(self._workspace_task)
        self._summary.update(
            self._summary_task,
            description=f"Progress: {self._format_results()}",
        )
        self.log(message)
        self._active_crate = None

    # ----------------------------------------------------------

    def fail(self, crate_name: str):
        if not self._rich_enabled:
            return

        elapsed = self.elapsed_seconds()

        self._progress.update(
            self._workspace_task,
            description=f"Failed at {crate_name} after {elapsed:.1f}s",
        )
        self._summary.update(
            self._summary_task,
            description=f"Progress: {self._format_results()}",
        )

    # ----------------------------------------------------------

    def interrupt(self):
        if not self._rich_enabled:
            return

        elapsed = self.elapsed_seconds()

        self._progress.update(
            self._workspace_task,
            description=f"Interrupted by user after {elapsed:.1f}s",
        )
        self._summary.update(
            self._summary_task,
            description=f"Progress: {self._format_results()}",
        )

    # ----------------------------------------------------------

    def finish(self):
        if not self._rich_enabled:
            return

        elapsed = self.elapsed_seconds()

        self._progress.update(
            self._workspace_task,
            description=f"Done in {elapsed:.1f}s",
        )
        self._summary.update(
            self._summary_task,
            description=f"Progress: {self._format_results()}",
        )

    # ----------------------------------------------------------

    def print_summary(
        self,
        failed_crates: Sequence[str],
        *,
        interrupted: bool = False,
        stopped_early: bool = False,
    ):
        elapsed = self.elapsed_seconds()
        status = "INTERRUPTED" if interrupted else "FAILED" if failed_crates else "OK"

        self.log(f"{self.tag} Summary ({status})")
        self.log(f"{self.tag} Elapsed: {elapsed:.1f}s")
        self.log(f"{self.tag} Results: {self._format_results()}")

        if interrupted:
            self.log(f"{self.tag} Interrupted by user.")
        elif stopped_early:
            self.log(f"{self.tag} Stopped at first failure.")

        if failed_crates:
            self.log(f"{self.tag} Failed crates:")
            for crate in failed_crates:
                self.log(f"  - {crate}")
