class AiClientBase:
    async def generate_text(
        self,
    ):
        raise NotImplementedError("This method should be overridden by subclasses.")
