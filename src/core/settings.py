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
    CERTIFICATIONS_DB_HOST: str
    CERTIFICATIONS_DB_PORT: int
    CERTIFICATIONS_DB_USER: str
    CERTIFICATIONS_DB_PASSWORD: str
    CERTIFICATIONS_DB_NAME: str
    CERTIFICATIONS_DB_SSLMODE: str = "require"  # Default SSL mode for PostgreSQL
    
    FERNET_KEY_SECRET: str  # Secret key for Fernet encryption, loaded from .env

    # Runtime mode is selected by the development/production launch script.
    environment: str = "development"


    # Reddit API settings

    REDDIT_CLIENT_ID: str | None = None
    REDDIT_CLIENT_SECRET: str | None = None  # keep None if using installed app grant
    REDDIT_DEVICE_ID: str | None = None      # if installed app, set any stable string
    REDDIT_SCOPE: str = "read"
    REDDIT_USER_AGENT: str 

    # StackExchange API settings
    STACKEXCHANGEOVERFLOW_API_KEY: str | None = None  # Optional, but helps with

    # Meetup API settings (does not provided need a pro subscription)
    # MEETUP_ACCESS_TOKEN: str | None = None  # OAuth2 Bearer token

    # aws 
    AWS_ACCESS_KEY_ID: str
    AWS_SECRET_ACCESS_KEY: str
    AWS_REGION: str = "ap-southeast-1"

    # Product Hunt API settings
    PRODUCTHUNT_DEVELOPER_TOKEN: str        # Bearer developer token (from Product Hunt app)
    PRODUCTHUNT_USER_AGENT: str = "quiz-certify/1.0 (+https://asodya.com)"

    # Gemini

    GEMINI_API_KEY: str | None = None

    # REDIS
    REDIS_URL: str = "redis://127.0.0.1:6379/0"
    REDIS_NAMESPACE: str = "certifications_api:"

    @property
    def development_mode(self) -> bool:
        return self.environment.lower() in {"development", "dev", "local"}

    QUESTIONS_PREFIX: str = "questions"

    


    @property
    def certifications_db(self) -> DatabaseConfig:
        return DatabaseConfig(
            dialect="postgresql",
            username=self.CERTIFICATIONS_DB_USER,
            password=self.CERTIFICATIONS_DB_PASSWORD,
            host=self.CERTIFICATIONS_DB_HOST,
            port=self.CERTIFICATIONS_DB_PORT,
            database=self.CERTIFICATIONS_DB_NAME,
            options={"sslmode": self.CERTIFICATIONS_DB_SSLMODE}
        )

    class Config:
        env_file = ".env"  # Optional with load_dotenv, but good for pydantic to know


# Singleton
@lru_cache()
def app_settings() -> Settings:
    return Settings()
