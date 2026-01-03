from src.core.logs import error
from src.dal.remote.ai.ai_factory import AiFactory
from src.domain.services.ai_token_manager import AiTokenManager

ai_factory = AiFactory()
ai_mananger = AiTokenManager()


def create_ai_token_for_user(
    user_uuid_id: str, token_name: str, token_value: str, is_default: bool
):
    saved_user_ai_token = ai_mananger.save_token(
        user_uuid_id=user_uuid_id,
        token_name=token_name,
        token_value=token_value,
        is_default=is_default,
    )
    return saved_user_ai_token


def get_user_ai_tokens_list(user_uuid: str):
    """
    Extracts the user token from the request headers.

    Args:
        request (Request): The incoming HTTP request.
    """

    user_tokens = ai_mananger.get_tokens(user_uuid_id=user_uuid)

    return user_tokens


def get_available_ai_providers_list():
    all_providers = list(ai_factory.ai_adapters.keys())

    return all_providers


def is_missing_important_fields(token_data: dict):
    important_fields = ["token_name", "token_value", "is_default"]

    missing_fields = []

    for field in important_fields:
        if field not in token_data:
            missing_fields.append(field)

    if missing_fields:
        error(f"Missing important fields: {missing_fields}")
        return True, missing_fields

    return False, None


def delete_token_for_user(user_uuid_id: str, token_name: str):
    ai_mananger.db_adapter.delete_where(
        "accredit_usertokens",
        {
            "token_name": token_name,
            "user_uuid_id": user_uuid_id,
        },
    )
    return True


def set_token_as_default_for_user(user_uuid_id: str, token_name: str):
    ai_mananger.set_default_token(user_uuid_id=user_uuid_id, token_name=token_name)

    return True
