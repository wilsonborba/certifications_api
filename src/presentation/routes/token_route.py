from fastapi import APIRouter, Request, Response, status
from fastapi.responses import JSONResponse

ai_token_router = APIRouter()

from src.core.logs import error
from src.presentation.handler.responses import MyResponse
from src.presentation.handler.tokens_handlers import (
    create_ai_token_for_user,
    delete_token_for_user,
    get_available_ai_providers_list,
    get_user_ai_tokens_list,
    is_missing_important_fields,
)


@ai_token_router.get("/tokens/available_providers", response_class=JSONResponse)
async def get_available_ai_providers(
    response: Response,
    request: Request,
):
    """
    Endpoint to get the list of available AI providers.
    """

    try:
        all_providers = get_available_ai_providers_list()

        response.status_code = status.HTTP_200_OK
        return MyResponse(
            data=all_providers,
            message="Available AI providers retrieved successfully.",
        )

    except Exception as e:
        error(f"Error retrieving AI providers: {e}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(
            data=None,
            message="Internal server error while retrieving AI providers.",
        )


@ai_token_router.get("/tokens/user_tokens", response_class=JSONResponse)
async def get_user_ai_tokens(
    response: Response,
    request: Request,
):
    """
    Endpoint to get the user's AI tokens.
    """

    try:
        user_uuid = request.headers.get("x-uuid", None)
        if not user_uuid:
            response.status_code = status.HTTP_400_BAD_REQUEST
            return MyResponse(
                data=None,
                message="User UUID not provided in headers.",
            )

        user_tokens = get_user_ai_tokens_list(user_uuid=user_uuid)

        response.status_code = status.HTTP_200_OK
        return MyResponse(
            data=user_tokens,
            message="User AI tokens retrieved successfully.",
        )

    except Exception as e:
        error(f"Error retrieving user AI tokens: {e}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(
            data=None,
            message="Internal server error while retrieving user AI tokens.",
        )


@ai_token_router.post("/tokens/create_token", response_class=JSONResponse)
async def create_user_ai_token(
    response: Response,
    request: Request,
):
    """
    Endpoint to create a new AI token for the user.

    class UserTokens(models.Model):
        id = models.BigAutoField(primary_key=True)
        user_uuid_id = models.UUIDField(db_index=True)
        token_name = models.CharField(max_length=255, db_index=True)
        model_version = models.CharField(max_length=100, null=True, blank=True)
        token_value = models.TextField()
        is_default = models.BooleanField(default=False)

        def __str__(self):
            return f"UserTokens[{self.pk}] for User[{self.user_uuid_id}] TokenName[{self.token_name}]"

        class Meta:
            # can only have one token_name per user and only one default token per user
            #
            # e.g., user_uuid_id + token_name must be unique
            # and user_uuid_id + is_default=True must be unique

            constraints = [
                models.UniqueConstraint(
                    fields=[
                        "user_uuid_id",
                        "token_name",
                    ],
                    name="uniq_user_token_name_per_user",
                ),
                models.UniqueConstraint(
                    fields=[
                        "user_uuid_id",
                    ],
                    condition=models.Q(is_default=True),
                    name="uniq_user_default_token_per_user",
                ),
            ]
    """

    try:
        user_uuid = request.headers.get("x-uuid", None)
        if not user_uuid:
            user_uuid = "00000000-0000-0000-0000-000000000000"

        # get body
        body = await request.json()

        token_name = body.get("token_name", None)
        token_value = body.get("token_value", None)
        model_version = body.get("model_version", None)
        is_default = body.get("is_default", None)

        missing_fields, missing_field_names = is_missing_important_fields(body)
        if missing_fields:
            response.status_code = status.HTTP_400_BAD_REQUEST
            return MyResponse(
                data=None,
                message=f"Missing important fields: {missing_field_names}",
            )

        saved_token = create_ai_token_for_user(
            user_uuid_id=user_uuid,
            token_name=token_name,
            token_value=token_value,
            is_default=is_default,
        )

        response.status_code = status.HTTP_201_CREATED
        return MyResponse(
            data=saved_token,
            message="User AI token created successfully.",
        )

    except Exception as e:
        error(f"Error creating user AI token: {e}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(
            data=None,
            message="Internal server error while creating user AI token.",
        )


@ai_token_router.delete(
    "/tokens/delete_token/{token_name}", response_class=JSONResponse
)
async def delete_user_ai_token(
    token_name: str,
    response: Response,
    request: Request,
):
    """
    Endpoint to delete a user AI token by its ID.
    """

    try:
        user_uuid = request.headers.get("x-uuid", None)
        if not user_uuid:
            response.status_code = status.HTTP_400_BAD_REQUEST
            return MyResponse(
                data=None,
                message="User UUID not provided in headers.",
            )

        delete_token_for_user(user_uuid_id=user_uuid, token_name=token_name)

        response.status_code = status.HTTP_200_OK
        return MyResponse(
            data=None,
            message="User AI token deleted successfully.",
        )

    except Exception as e:
        error(f"Error deleting user AI token: {e}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(
            data=None,
            message="Internal server error while deleting user AI token.",
        )
