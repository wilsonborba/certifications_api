

# from django.db import models
# from .user_certification import UserCertification  # Assuming this model exists
# from .source_question import Question  # Assuming this model exists
# from .source_answer import Answer  # Assuming this model exists

# class UserAnswer(models.Model):
#     # Reference to the user certification
#     user_certification = models.ForeignKey(UserCertification, on_delete=models.CASCADE)

#     # Reference to the question
#     question = models.ForeignKey(Question, on_delete=models.CASCADE)

#     # Reference to the correct answer
#     correct_answer = models.ForeignKey(Answer, on_delete=models.CASCADE, )

#     # Reference to the selected answer (nullable)
#     selected_answer = models.ForeignKey(Answer, on_delete=models.CASCADE, null=True, blank=True)

#     # Reference to the user (assuming a User model exists)
#     user_uuid_id = models.UUIDField(db_index=True)

#     updated_at = models.DateTimeField(auto_now=True)

#     def __str__(self):
#         return f"UserAnswer[{self.pk}] for User[{self.user_id}] Question[{self.question_id}]"

from dataclasses import asdict, dataclass, field

@dataclass
class UserAnswerModel:
    user_certification_id: int | None = None
    question_id: int | None = None
    correct_answer_id: int | None = None
    selected_answer_id: int | None = None
    is_correct: bool | None = None
    user_uuid_id: str | None = None
    updated_at: str | None = None

    def to_dict(self):
        return asdict(self)