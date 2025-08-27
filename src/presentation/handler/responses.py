from typing import Optional, Any
from fastapi import status
from pydantic import BaseModel


class MyResponse(BaseModel):
    """
    Base response model for API responses.
    responde status from fastapi.status
    - https://fastapi.tiangolo.com/advanced/status-codes/
    - https://fastapi.tiangolo.com/tutorial/response-status-code/

    """
    status: int
    message: Optional[str] = None
    data: Optional[Any] = None

    def __init__(self, status: str, message: Optional[str] = None, data: Optional[dict] = None):
        super().__init__(status=status, message=message, data=data)


    def to_dict(self) -> dict:
        """
        Convert the response model to a dictionary.
        """
        return {
            "status": self.status,
            "message": self.message,
            "data": self.data
        }
    