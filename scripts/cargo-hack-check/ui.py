from __future__ import annotations

from sys import stderr
from contextlib import suppress
import time
from typing import Optional


class CargoHackDashboard:
    def __init__(self, total_crates: int, tag: str):
        self.total_crates = total_crates
        self.tag = tag

        self.success = 0
        self.skipped = 0
        self.failed = 0
        self.cache_hits = 0

        self._start_time = time.monotonic()

        self._rich_enabled = self._should_use_rich()

        self._progress = None
        self._workspace_task: Optional[int] = None
        self._console = None

        if self._rich_enabled:
            self._init_rich()

    # ----------------------------------------------------------

    def _should_use_rich(self) -> bool:
        if not stderr.isatty():
            return False

        with suppress(Exception):
            from rich.console import Console
            return Console(file=stderr).is_terminal

        return False

    # ----------------------------------------------------------

    def _init_rich(self):
        from rich.console import Console
        from rich.progress import (
            Progress,
            SpinnerColumn,
            BarColumn,
            TextColumn,
            TimeElapsedColumn,
            TimeRemainingColumn,
        )

        self._console = Console(
            file=stderr,
            force_terminal=True,
            force_interactive=True,
        )

        self._progress = Progress(
            SpinnerColumn(),
            TextColumn("{task.description}"),
            BarColumn(),
            TextColumn("{task.completed}/{task.total}"),
            TimeElapsedColumn(),
            TimeRemainingColumn(),
            console=self._console,
            refresh_per_second=12,
        )

        self._progress.start()

        self._workspace_task = self._progress.add_task(
            "",
            total=self.total_crates,
        )

    # ----------------------------------------------------------

    def log(self, message: str):
        if self._rich_enabled:
            self._console.print(message)
        else:
            print(message, file=stderr)

    # ----------------------------------------------------------

    def close(self):
        if self._rich_enabled and self._progress:
            self._progress.stop()

    # ----------------------------------------------------------

    def start_crate(self, crate_name: str, index: int):
        if not self._rich_enabled:
            return

        self._progress.update(
            self._workspace_task,
            description=(
                f"{self.tag} [{index}/{self.total_crates}] {crate_name} "
                f"ok={self.success} skip={self.skipped} fail={self.failed} cache={self.cache_hits}"
            ),
        )

    # ----------------------------------------------------------

    def finish_crate(self, *, skipped: bool, success: bool):
        if skipped:
            self.skipped += 1
            self.cache_hits += 1
        elif success:
            self.success += 1
        else:
            self.failed += 1

        if not self._rich_enabled:
            return

        self._progress.advance(self._workspace_task)

    # ----------------------------------------------------------

    def fail(self, crate_name: str):
        if not self._rich_enabled:
            return

        elapsed = time.monotonic() - self._start_time

        self._progress.update(
            self._workspace_task,
            description=(
                f"{self.tag} Failed at {crate_name} after {elapsed:.1f}s "
                f"ok={self.success} skip={self.skipped} fail={self.failed} cache={self.cache_hits}"
            ),
        )

    # ----------------------------------------------------------

    def finish(self):
        if not self._rich_enabled:
            return

        elapsed = time.monotonic() - self._start_time

        self._progress.update(
            self._workspace_task,
            description=(
                f"{self.tag} Done in {elapsed:.1f}s "
                f"ok={self.success} skip={self.skipped} fail={self.failed} cache={self.cache_hits}"
            ),
        )
