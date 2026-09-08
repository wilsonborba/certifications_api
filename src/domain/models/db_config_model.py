from dataclasses import dataclass, field

@dataclass
class DatabaseConfig:
    dialect: str           # e.g. 'postgresql', 'mysql', 'sqlite'
    username: str          # e.g. 'user'
    password: str          # e.g. 'pass'
    host: str              # e.g. 'localhost'
    port: int              # e.g. 5432
    database: str          # e.g. 'my_db'
    options: dict[str, str] = field(default_factory=dict)

    def uri(self) -> str:
        if self.dialect == "sqlite":
            return f"sqlite:///{self.database}"

        base = f"{self.dialect}://{self.username}:{self.password}@{self.host}:{self.port}/{self.database}"

        if self.options:
            query = "&".join(f"{key}={value}" for key, value in self.options.items())
            return f"{base}?{query}"
        return base