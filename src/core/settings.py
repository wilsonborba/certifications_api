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


    # Gemini

    GEMINI_API_KEY: str | None = None

    # REDIS
    REDIS_URL: str = "redis://127.0.0.1:6379/0"
    REDIS_NAMESPACE: str = "certifications_api:"

    @property
    def development_mode(self) -> bool:
        return self.environment.lower() in {"development", "dev", "local"}

    QUESTIONS_PREFIX: str = "questions"

    # Cortex is a private service dependency.  Its address is deliberately a
    # non-secret runtime setting; credentials, if Cortex later requires them,
    # belong in .env rather than in this class.
    CORTEX_BASE_URL: str = "http://127.0.0.1:8003"
    CORTEX_TIMEOUT_SECONDS: float = 45.0
    CORTEX_TENANT_ID: str = "certifications"

    # Product safeguards, not a second Cortex availability manager. Cortex
    # owns model routing/cooldowns; Certifications only limits its own work.
    GENERATION_EASY_DAILY_LIMIT: int = 1
    GENERATION_PREMIUM_DAILY_LIMIT: int = 1
    GENERATION_T0_GLOBAL_CONCURRENCY: int = 2
    GENERATION_PREMIUM_GLOBAL_CONCURRENCY: int = 1
    GENERATION_LEASE_SECONDS: int = 15 * 60

    


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
