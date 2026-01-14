from datetime import datetime, timezone
from typing import Optional

from fastapi import APIRouter, Query, Request, Response, status
from fastapi.responses import JSONResponse

ai_token_router = APIRouter()

from src.core.logs import error
from src.presentation.handler.responses import MyResponse
from src.presentation.handler.tokens_handlers import (
    ai_factory,
    create_ai_token_for_user,
    delete_token_for_user,
    get_available_ai_providers_list,
    get_user_ai_tokens_list,
    get_user_ai_usage,
    is_missing_important_fields,
    set_token_as_default_for_user,
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
            data=all_providers,  # pyright: ignore[reportArgumentType]
            message="Available AI providers retrieved successfully.",
        )

    except Exception as e:
        error(f"Error retrieving AI providers: {e}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(
            data=None,
            message="Internal server error while retrieving AI providers.",
        )


@ai_token_router.get("/tokens/usage", response_class=JSONResponse)
async def get_ai_usage(
    response: Response,
    request: Request,
    provider_model_description: Optional[str] = Query(
        default=None,
        alias="provider_model_description",
    ),
    start_date: Optional[datetime] = Query(default=None, alias="start_date"),
    end_date: Optional[datetime] = Query(default=None, alias="end_date"),
):
    """
    Endpoint to get AI usage information.
    """
    try:
        user_uuid_id = request.headers.get("x-uuid")
        if not user_uuid_id:
            response.status_code = status.HTTP_400_BAD_REQUEST
            return MyResponse(data=None, message="User UUID not provided in headers.")

        # Optional: enforce start_date <= end_date if both provided
        if start_date and end_date and start_date > end_date:
            response.status_code = status.HTTP_400_BAD_REQUEST
            return MyResponse(data=None, message="start_date must be <= end_date.")

        usage_info = get_user_ai_usage(
            user_uuid_id=user_uuid_id,
            provider_model_description=provider_model_description,
            start_date=start_date,
            end_date=end_date,
        )

        response.status_code = status.HTTP_200_OK
        return MyResponse(
            data=usage_info,
            message="AI usage information retrieved successfully.",
        )

    except Exception as e:
        error(f"Error retrieving AI usage information: {e}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(
            data=None,
            message="Internal server error while retrieving AI usage information.",
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

    """

    try:
        user_uuid = request.headers.get("x-uuid", None)
        if not user_uuid:
            user_uuid = "00000000-0000-0000-0000-000000000000"

        # get body
        body = await request.json()

        token_name = body.get("token_name", None)
        token_value = body.get("token_value", None)
        # model_version = body.get("model_version", None)
        is_default = body.get("is_default", None)
        provider_name = body.get("provider_name", None)

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
            provider_name=provider_name,
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

        response.status_code = status.HTTP_202_ACCEPTED
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


@ai_token_router.patch("/tokens/set_default", response_class=JSONResponse)
async def set_default_user_ai_token(
    response: Response,
    request: Request,
):
    """
    Endpoint to set a user AI token as default by its name.
    """

    body = await request.json()

    token_name = body.get("token_name", None)

    if not token_name:
        response.status_code = status.HTTP_400_BAD_REQUEST
        return MyResponse(
            data=None,
            message="Token name not provided in the request body.",
        )

    try:
        user_uuid = request.headers.get("x-uuid", None)
        if not user_uuid:
            response.status_code = status.HTTP_400_BAD_REQUEST
            return MyResponse(
                data=None,
                message="User UUID not provided in headers.",
            )

        set_token_as_default_for_user(user_uuid_id=user_uuid, token_name=token_name)

        response.status_code = status.HTTP_202_ACCEPTED
        return MyResponse(
            data=None,
            message="User AI token set as default successfully.",
        )

    except Exception as e:
        error(f"Error setting user AI token as default: {e}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(
            data=None,
            message="Internal server error while setting user AI token as default.",
        )
