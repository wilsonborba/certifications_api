# certifications_api

Backend API and Ingestion Engine for Asodya Certifications.

## Overview

`certifications_api` powers the core exam simulation, question governance, multimodal document/media ingestion, and quiz evaluation workflows for the Asodya Certifications platform.

## Key Features

- **Multi-Modal Document & Media Ingestion**: End-to-end ingestion pipeline supporting PDFs, documents, and media streams with intelligent text extraction and slicing.
- **Dynamic Quiz & Exam Generation**: AI-driven quiz evaluation, question governance, scoring, and performance analytics.
- **Request Tracing & Telemetry**: Built-in `RequestIdMiddleware` generating and propagating UUID `X-Request-ID` across all response headers and exception payloads.
- **Public Waitlist & Access Control**: Seamless waitlist registration contracts and anonymous/quota tier governance.

## Architecture & Tech Stack

- **Runtime**: Python 3.12+ / FastAPI / Pydantic
- **Databases & Cache**:
  - PostgreSQL (Alembic migrations for studies, questions, attempts, and waitlists)
  - Redis (caching and rate limits)
- **Deployment**: Managed as a systemd service (`certifications-api.service`).

## Getting Started

### Prerequisites

- Python 3.12+
- `uv` or `pip`
- PostgreSQL & Redis

### Installation

```bash
git clone git@github.com:wilsonborba/certifications_api.git
cd certifications_api

uv venv
source .venv/bin/activate
uv pip install -e .
```

### Running Locally

```bash
uvicorn src.main:app --host 0.0.0.0 --port 8001 --reload
```

## Changelog & Releases

Changelogs are maintained and generated using [git-cliff](https://git-cliff.org).

```bash
git cliff -o CHANGELOG.md --tag <tag>
```

## License

Proprietary © Asodya. All rights reserved.
