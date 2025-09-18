from typing import Optional, Any
from fastapi import status
from pydantic import BaseModel


class MalwareDetectedError(Exception):
    """Custom exception for malware detection."""
    pass

class UnsupportedFileTypeError(Exception):
    """Custom exception for unsupported file types."""
    pass



class MyResponse(BaseModel):
    """
    Base response model for API responses.
    responde status from fastapi.status
    - https://fastapi.tiangolo.com/advanced/status-codes/
    - https://fastapi.tiangolo.com/tutorial/response-status-code/

    """
    message: Optional[str] = None
    data: Optional[Any] = None

    def __init__(self,  message: Optional[str] = None, data: Optional[dict] = None):
        super().__init__( message=message, data=data)



    