# src/core/settings.py

from pydantic_settings import BaseSettings
from functools import lru_cache
from dotenv import load_dotenv

from src.domain.models.db_config_model import DatabaseConfig

load_dotenv()  # Loads .env file

class Settings(BaseSettings):
    # API
    API_KEY_NAME: str = "Authorization"
    API_KEY_SECRET: str  # Will be loaded from .env
    ACCREDIT_DB_HOST: str
    ACCREDIT_DB_PORT: int
    ACCREDIT_DB_USER: str
    ACCREDIT_DB_PASSWORD: str
    ACCREDIT_DB_NAME: str
    ACCREDIT_DB_SSLMODE: str = "require"  # Default SSL mode for PostgreSQL
    
    FERNET_KEY_SECRET: str  # Secret key for Fernet encryption, loaded from .env

    # Development flag
    development_mode: bool = True


    # Reddit API settings

    REDDIT_CLIENT_ID: str | None = None
    REDDIT_CLIENT_SECRET: str | None = None  # keep None if using installed app grant
    REDDIT_DEVICE_ID: str | None = None      # if installed app, set any stable string
    REDDIT_SCOPE: str = "read"
    REDDIT_USER_AGENT: str 

    # StackExchange API settings
    STACKEXCHANGEOVERFLOW_API_KEY: str | None = None  # Optional, but helps with

    # Meetup API settings
    MEETUP_ACCESS_TOKEN: str | None = None  # OAuth2 Bearer token

    @property
    def accredit_db(self) -> DatabaseConfig:
        return DatabaseConfig(
            dialect="postgresql",
            username=self.ACCREDIT_DB_USER,
            password=self.ACCREDIT_DB_PASSWORD,
            host=self.ACCREDIT_DB_HOST,
            port=self.ACCREDIT_DB_PORT,
            database=self.ACCREDIT_DB_NAME,
            options={"sslmode": self.ACCREDIT_DB_SSLMODE}
        )

    class Config:
        env_file = ".env"  # Optional with load_dotenv, but good for pydantic to know


# Singleton
@lru_cache()
def app_settings() -> Settings:
    return Settings()