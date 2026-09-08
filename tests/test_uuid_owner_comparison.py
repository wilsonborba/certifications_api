from __future__ import annotations

import unittest

from src.presentation.routes.quiz_governance_route import _same_user


class SameUserTests(unittest.TestCase):
    """api_for_apps's gateway forwards x-uuid undashed (straight from the
    session's user_uuid_id), while Postgres's native uuid columns always
    read back canonical-dashed. A plain string `==`/`!=` between the two
    would treat the real owner as a stranger; _same_user must not."""

    def test_dashed_and_undashed_forms_of_the_same_uuid_are_equal(self) -> None:
        dashed = "52ce89e3-ad74-c8b6-c76c-e531b7a4eba0"
        undashed = "52ce89e3ad74c8b6c76ce531b7a4eba0"
        self.assertTrue(_same_user(dashed, undashed))
        self.assertTrue(_same_user(undashed, dashed))

    def test_uppercase_form_is_also_equal(self) -> None:
        self.assertTrue(_same_user("52CE89E3AD74C8B6C76CE531B7A4EBA0", "52ce89e3-ad74-c8b6-c76c-e531b7a4eba0"))

    def test_different_uuids_are_not_equal(self) -> None:
        self.assertFalse(_same_user("52ce89e3-ad74-c8b6-c76c-e531b7a4eba0", "00000000-0000-0000-0000-000000000000"))

    def test_none_is_only_equal_to_none(self) -> None:
        self.assertTrue(_same_user(None, None))
        self.assertFalse(_same_user(None, "52ce89e3ad74c8b6c76ce531b7a4eba0"))
        self.assertFalse(_same_user("52ce89e3ad74c8b6c76ce531b7a4eba0", None))

    def test_a_non_uuid_value_falls_back_to_plain_equality_instead_of_raising(self) -> None:
        self.assertTrue(_same_user("not-a-uuid", "not-a-uuid"))
        self.assertFalse(_same_user("not-a-uuid", "52ce89e3ad74c8b6c76ce531b7a4eba0"))


if __name__ == "__main__":
    unittest.main()
