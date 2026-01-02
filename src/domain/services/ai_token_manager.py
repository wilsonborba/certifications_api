from src.dal.local.db_adapter import DBAdapter


class AiTokenManager:
    def __init__(self):
        self.db_adapter = DBAdapter()

    def get_tokens(self, user_uuid_id):
        db_token = self.db_adapter.read_where_many(
            "accredit_usertokens",
            {"user_uuid_id": user_uuid_id},
        )

        return db_token

    def get_unique_token(
        self,
        user_uuid_id: str,
        token_name: str,
    ):
        db_token = self.db_adapter.read_where_one(
            "accredit_usertokens",
            {
                "user_uuid_id": user_uuid_id,
                "token_name": token_name,
            },
        )

        return db_token

    def get_default_token(self, user_uuid_id: str):
        db_token = self.db_adapter.read_where_one(
            "accredit_usertokens",
            {
                "user_uuid_id": user_uuid_id,
                "is_default": True,
            },
        )

        return db_token

    def save_token(
        self, user_uuid_id: str, token_name: str, token_value: str, is_default: bool
    ):
        existing_token = self.get_unique_token(user_uuid_id, token_name)

        # need to get the default token to unset it if the new one is set as default
        # only if is_default is True
        # this is to ensure only one default token per user
        if is_default:
            default_token = self.get_default_token(user_uuid_id)
            if default_token and default_token["token_name"] != token_name:
                self.db_adapter.update_row(
                    "accredit_usertokens",
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
                "accredit_usertokens",
                existing_token["id"],
                {
                    "token_value": token_value,
                    "is_default": is_default,
                },
            )
            return self.get_unique_token(user_uuid_id, token_name)  # dict

        else:
            # Insert new token
            new_token_id = self.db_adapter.insert_row(
                "accredit_usertokens",
                {
                    "user_uuid_id": user_uuid_id,
                    "token_name": token_name,
                    "token_value": token_value,
                    "is_default": is_default,
                },
            )

            return self.db_adapter.read_by_id("accredit_usertokens", new_token_id)

    def set_default_token(self, user_uuid_id: str, token_name: str):
        # First, unset any existing default token for the user
        self.db_adapter.update_where(
            "accredit_usertokens",
            {"is_default": False},
            {"user_uuid_id": user_uuid_id, "is_default": True},
        )

        # Then, set the specified token as default
        self.db_adapter.update_where(
            "accredit_usertokens",
            {"is_default": True},
            {"user_uuid_id": user_uuid_id, "token_name": token_name},
        )

    def delete_token(self, user_uuid_id: str, token_name: str):
        self.db_adapter.delete_where(
            "accredit_usertokens",
            {"user_uuid_id": user_uuid_id, "token_name": token_name},
        )
