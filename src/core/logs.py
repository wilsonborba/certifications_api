from __future__ import annotations

import asyncio
import inspect
import logging
import logging.handlers
import os
import sys
from enum import Enum
from functools import partial
from pathlib import Path
from typing import AsyncIterator


class LogTarget(str, Enum):
    CLI = "cli"
    API = "api"


RESET = "\033[0m"
LEVEL_COLORS = {
    logging.DEBUG: "\033[38;5;244m",
    logging.INFO: "\033[38;5;39m",
    logging.WARNING: "\033[38;5;220m",
    logging.ERROR: "\033[38;5;196m",
    logging.CRITICAL: "\033[1;38;5;196m",
}
SOURCE_COLOR = "\033[38;5;45m"
TIMESTAMP_COLOR = "\033[38;5;250m"
MESSAGE_COLOR = "\033[38;5;255m"

LOG_FORMAT = "%(asctime)s | %(levelname)-8s | %(name)s | %(funcName)s:%(lineno)d | %(message)s"
DATE_FORMAT = "%Y-%m-%d %H:%M:%S"

# Bounded log files for rotating storage (5MB per file, 3 backups, max 20MB total)
LOG_FILE_MAX_BYTES = 5 * 1024 * 1024
LOG_FILE_BACKUP_COUNT = 3
POLL_INTERVAL_SECONDS = 0.3

_MANAGED_HANDLER_ATTR = "_certifications_managed_handler"


class StructuredFormatter(logging.Formatter):
    def __init__(self, *, use_color: bool) -> None:
        super().__init__(fmt=LOG_FORMAT, datefmt=DATE_FORMAT)
        self.use_color = use_color

    def format(self, record: logging.LogRecord) -> str:
        timestamp = self.formatTime(record, self.datefmt)
        level = f"{record.levelname:<8}"
        source = f"{record.name}"
        location = f"{record.funcName}:{record.lineno}"
        message = record.getMessage()

        if record.exc_info:
            exc_text = self.formatException(record.exc_info)
            message = f"{message}\n{exc_text}"

        if not self.use_color:
            return f"{timestamp} | {level} | {source} | {location} | {message}"

        timestamp_str = f"{TIMESTAMP_COLOR}{timestamp}{RESET}"
        level_str = f"{LEVEL_COLORS.get(record.levelno, '')}{level}{RESET}"
        source_str = f"{SOURCE_COLOR}{source}{RESET}"
        location_str = f"{SOURCE_COLOR}{location}{RESET}"
        message_str = f"{MESSAGE_COLOR}{message}{RESET}"
        return f"{timestamp_str} | {level_str} | {source_str} | {location_str} | {message_str}"


class CliFilter(logging.Filter):
    def __init__(self, verbose: bool) -> None:
        super().__init__()
        self.verbose = verbose

    def filter(self, record: logging.LogRecord) -> bool:
        if self.verbose:
            return True
        return record.levelno >= logging.WARNING


def _should_use_color(target: LogTarget) -> bool:
    if os.getenv("NO_COLOR"):
        return False
    stream = sys.stderr if target == LogTarget.CLI else sys.stdout
    return hasattr(stream, "isatty") and stream.isatty()


def _resolve_level(*, debug: bool, verbose: bool, target: LogTarget) -> int:
    if debug or verbose:
        return logging.DEBUG
    if target == LogTarget.API:
        return logging.INFO
    return logging.WARNING


def configure_logging(
    *,
    debug: bool = False,
    verbose: bool = False,
    target: LogTarget = LogTarget.CLI,
    log_file: Path | None = None,
) -> None:
    level = _resolve_level(debug=debug, verbose=verbose, target=target)
    root_logger = logging.getLogger()

    for handler in list(root_logger.handlers):
        if getattr(handler, _MANAGED_HANDLER_ATTR, False):
            root_logger.removeHandler(handler)

    root_logger.setLevel(logging.DEBUG)

    stream = sys.stderr if target == LogTarget.CLI else sys.stdout
    stream_handler = logging.StreamHandler(stream)
    stream_handler.setLevel(level)
    stream_handler.setFormatter(StructuredFormatter(use_color=_should_use_color(target)))
    if target == LogTarget.CLI:
        stream_handler.addFilter(CliFilter(verbose=verbose))
    setattr(stream_handler, _MANAGED_HANDLER_ATTR, True)
    root_logger.addHandler(stream_handler)

    if log_file is not None:
        log_file.parent.mkdir(parents=True, exist_ok=True)
        file_handler = logging.handlers.RotatingFileHandler(
            log_file,
            maxBytes=LOG_FILE_MAX_BYTES,
            backupCount=LOG_FILE_BACKUP_COUNT,
            encoding="utf-8",
        )
        file_handler.setLevel(logging.DEBUG)
        file_handler.setFormatter(StructuredFormatter(use_color=False))
        setattr(file_handler, _MANAGED_HANDLER_ATTR, True)
        root_logger.addHandler(file_handler)

    for logger_name in ["uvicorn", "uvicorn.error", "uvicorn.access", "httpx"]:
        third_party = logging.getLogger(logger_name)
        third_party.setLevel(logging.INFO if target == LogTarget.API else logging.WARNING)
        third_party.propagate = True


def get_logger(name: str) -> logging.Logger:
    return logging.getLogger(name)


async def _tail(path: Path, *, poll_interval: float = POLL_INTERVAL_SECONDS) -> AsyncIterator[str]:
    already_had_content = path.exists()
    while not path.exists():
        await asyncio.sleep(poll_interval)
    handle = path.open("r", errors="replace", encoding="utf-8")
    try:
        if already_had_content:
            handle.seek(0, 2)
        position = handle.tell()
        while True:
            line = handle.readline()
            if line:
                position = handle.tell()
                yield line.rstrip("\n")
                continue
            if path.exists() and path.stat().st_size < position:
                handle.close()
                handle = path.open("r", errors="replace", encoding="utf-8")
                position = 0
            await asyncio.sleep(poll_interval)
    finally:
        handle.close()


# === Logging utility functions ===
# Inspects the caller's frame so log lines show the actual call site.

def log_message(level: int, message: str) -> None:
    _logger = get_logger("certifications")
    frame = inspect.currentframe()
    while frame:
        if frame.f_globals["__name__"] != __name__:
            break
        frame = frame.f_back

    if frame:
        filename = frame.f_code.co_filename.split("/")[-1]
        function_name = frame.f_code.co_name
        line_number = frame.f_lineno
        formatted_message = f"[{filename} -> {function_name}():{line_number}] {message}"
    else:
        formatted_message = f"[unknown location] {message}"

    _logger.log(level, formatted_message)


# === Partial logging shortcuts ===
debug = partial(log_message, logging.DEBUG)
info = partial(log_message, logging.INFO)
warning = partial(log_message, logging.WARNING)
error = partial(log_message, logging.ERROR)
critical = partial(log_message, logging.CRITICAL)

# Backward-compat alias
logger = get_logger("certifications")
