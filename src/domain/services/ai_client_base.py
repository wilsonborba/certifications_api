class AiClientBase:
    def set_api_key(
        self,
        api_key: str,
    ):
        raise NotImplementedError("This method should be overridden by subclasses.")

    async def generate_text(
        self,
    ):
        raise NotImplementedError("This method should be overridden by subclasses.")
