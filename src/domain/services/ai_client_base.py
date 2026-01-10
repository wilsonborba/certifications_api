class AiClientBase:
    def __init__(self):
        self._last_status_code: int | None = None
        self._last_attempts: int = 0
        self._last_latency_ms: float = 0.0

    @property
    def last_status_code(self) -> int | None:
        return self._last_status_code

    @property
    def last_attempts(self) -> int:
        return self._last_attempts

    @property
    def last_latency_ms(self) -> float:
        return self._last_latency_ms

    def set_api_key(
        self,
        api_key: str,
    ):
        raise NotImplementedError("This method should be overridden by subclasses.")

    async def generate_text(
        self,
    ):
        raise NotImplementedError("This method should be overridden by subclasses.")
