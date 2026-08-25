from datetime import datetime
from typing import Any, Dict, Optional

from src.dal.local.db_adapter import DBAdapter


class AiTokenManager:
    def __init__(self):
        self.db_adapter = DBAdapter()

    def get_user_ai_usage(
        self,
        user_uuid_id: str,
        provider_model_description: str | None = None,
        start_date: datetime | None = None,
        end_date: datetime | None = None,
    ):
        where = {"user_uuid_id": user_uuid_id}

        if provider_model_description:
            where["provider_model_description"] = provider_model_description

        # IMPORTANT: use range operators on created_at
        if start_date or end_date:
            where["created_at"] = {}
            if start_date:
                where["created_at"]["$gte"] = start_date
            if end_date:
                where["created_at"]["$lte"] = end_date

        return self.db_adapter.read_where_many("certifications_aiusageevent", where)

    def get_tokens(self, user_uuid_id):
        db_token = self.db_adapter.read_where_many(
            "certifications_usertokens",
            {"user_uuid_id": user_uuid_id},
        )

        return db_token

    def get_unique_token(
        self,
        user_uuid_id: str,
        token_name: str,
    ):
        db_token = self.db_adapter.read_where_one(
            "certifications_usertokens",
            {
                "user_uuid_id": user_uuid_id,
                "token_name": token_name,
            },
        )

        return db_token

    def get_default_token(self, user_uuid_id: str):
        db_token = self.db_adapter.read_where_one(
            "certifications_usertokens",
            {
                "user_uuid_id": user_uuid_id,
                "is_default": True,
            },
        )

        return db_token

    def save_token(
        self,
        user_uuid_id: str,
        token_name: str,
        token_value: str,
        is_default: bool,
        provider_name: str,
    ):
        existing_token = self.get_unique_token(user_uuid_id, token_name)

        # need to get the default token to unset it if the new one is set as default
        # only if is_default is True
        # this is to ensure only one default token per user
        if is_default:
            default_token = self.get_default_token(user_uuid_id)
            if default_token and default_token["token_name"] != token_name:
                self.db_adapter.update_row(
                    "certifications_usertokens",
                    default_token["id"],
                    {
                        "is_default": False,
                    },
                )

        # Update or insert the token
        # If the token already exists, update it; otherwise, insert a new one
        # Return the token ID

        if existing_token:
            # Update existing token
            self.db_adapter.update_row(
                "certifications_usertokens",
                existing_token["id"],
                {
                    "token_value": token_value,
                    "is_default": is_default,
                    "provider_name": provider_name,
                },
            )
            return self.get_unique_token(user_uuid_id, token_name)  # dict

        else:
            # Insert new token
            new_token_id = self.db_adapter.insert_row(
                "certifications_usertokens",
                {
                    "user_uuid_id": user_uuid_id,
                    "token_name": token_name,
                    "token_value": token_value,
                    "is_default": is_default,
                    "provider_name": provider_name,
                },
            )

            return self.db_adapter.read_by_id("certifications_usertokens", new_token_id)

    def set_default_token(self, user_uuid_id: str, token_name: str):
        # 1) Unset current default ONLY for this user
        self.db_adapter.update_where(
            "certifications_usertokens",
            where={
                "user_uuid_id": user_uuid_id,
                "is_default": True,
            },
            data={"is_default": False},
        )

        # 2) Set the chosen token as default
        updated = self.db_adapter.update_where(
            "certifications_usertokens",
            where={
                "user_uuid_id": user_uuid_id,
                "token_name": token_name,
            },
            data={"is_default": True},
        )

        if updated == 0:
            raise ValueError("Token not found for user")

    def delete_token(self, user_uuid_id: str, token_name: str):
        self.db_adapter.delete_where(
            "certifications_usertokens",
            {"user_uuid_id": user_uuid_id, "token_name": token_name},
        )
