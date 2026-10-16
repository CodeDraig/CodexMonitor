import { memo } from "react";
import Check from "lucide-react/dist/esm/icons/check";
import type { ConversationItem } from "@/types";

type UserInputRowProps = {
  item: Extract<ConversationItem, { kind: "userInput" }>;
  isExpanded: boolean;
  onToggle: (id: string) => void;
};

export const UserInputRow = memo(function UserInputRow({
  item,
  isExpanded,
  onToggle,
}: UserInputRowProps) {
  const first = item.questions[0];
  const previewQuestion =
    first?.question?.trim() || first?.header?.trim() || "Input requested";
  const firstAnswer = first?.answers[0]?.trim() || "No answer provided";
  const previewAnswer =
    first && first.answers.length > 1
      ? `${firstAnswer} +${first.answers.length - 1}`
      : firstAnswer;
  const extraQuestions = Math.max(0, item.questions.length - 1);

  return (
    <div className={`tool-inline user-input-inline ${isExpanded ? "tool-inline-expanded" : ""}`}>
      <button
        type="button"
        className="tool-inline-bar-toggle"
        onClick={() => onToggle(item.id)}
        aria-expanded={isExpanded}
        aria-label="Toggle answered input details"
      />
      <div className="tool-inline-content">
        <button
          type="button"
          className="tool-inline-summary tool-inline-toggle"
          onClick={() => onToggle(item.id)}
          aria-expanded={isExpanded}
        >
          <Check className="tool-inline-icon completed" size={14} aria-hidden />
          <span className="tool-inline-label">answered:</span>
          <span className="tool-inline-value user-input-inline-preview">
            {previewQuestion}: {previewAnswer}
            {extraQuestions > 0 ? ` +${extraQuestions} more` : ""}
          </span>
        </button>
        {isExpanded && (
          <div className="user-input-inline-details">
            {item.questions.map((question, index) => {
              const title = question.question || question.header || `Question ${index + 1}`;
              return (
                <div
                  key={`${question.id}-${index}`}
                  className="user-input-inline-entry"
                >
                  <div className="user-input-inline-question">{title}</div>
                  {question.answers.length > 0 ? (
                    <div className="user-input-inline-answers">
                      {question.answers.map((answer, answerIndex) => (
                        <div
                          key={`${question.id}-answer-${answerIndex}`}
                          className="user-input-inline-answer"
                        >
                          {answer}
                        </div>
                      ))}
                    </div>
                  ) : (
                    <div className="user-input-inline-empty-answer">
                      No answer provided.
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
});
