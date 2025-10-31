# from enum import Enum
# from pydantic import BaseModel

from enum import Enum
from pydantic import BaseModel

class LanguageEnum(str, Enum):
	ENGLISH = 'English'
	PORTUGUESE = 'Português'
	SPANISH = 'Español'
	FRENCH = 'Français'
	GERMAN = 'Deutsch'
	THAI = 'ไทย'
	JAPANESE = '日本語'
	KOREAN = '한국어'
	CHINESE_SIMPLIFIED = '中文 (简体)'
	CHINESE_TRADITIONAL = '中文 (繁體)'
	HINDI = 'हिन्दी'
	ARABIC = 'العربية'

def is_valid_language(language: str) -> bool:
    try:
        LanguageEnum(language)
        return True
    except ValueError:
        return False