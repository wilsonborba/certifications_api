import logging

import inspect
from functools import partial

from colorlog import ColoredFormatter


from src.core.settings import app_settings




# === Define log format and formatter ===
LOG_FORMAT = "%(asctime)s [%(log_color)s%(levelname)s%(reset)s] %(message)s"

formatter = ColoredFormatter(
    LOG_FORMAT,
    log_colors={
        "DEBUG": "cyan",
        "INFO": "green",
        "WARNING": "yellow",
        "ERROR": "red",
        "CRITICAL": "bold_red",
    },
    reset=True,
    style="%",
)

# === Console handler ===
handler = logging.StreamHandler()
handler.setFormatter(formatter)


# === Logger setup ===



settings = app_settings()
logger_level = logging.DEBUG if settings.development_mode else logging.INFO


logger = logging.getLogger("app_logger")
logger.setLevel(logger_level)
logger.addHandler(handler)

logger.propagate = False  # Prevent duplicate logs

# === Logging utility functions ===
def log_message(level, message):
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

    logger.log(level, formatted_message)



# === Partial logging shortcuts ===
debug = partial(log_message, logging.DEBUG)
info = partial(log_message, logging.INFO)
warning = partial(log_message, logging.WARNING)
error = partial(log_message, logging.ERROR)
critical = partial(log_message, logging.CRITICAL)
