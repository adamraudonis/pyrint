from collections import defaultdict
from dataclasses import dataclass, field


@dataclass(slots=True)
class RuntimeEntryData:
    state: defaultdict[type, dict[int, str]] = field(
        default_factory=lambda: defaultdict(dict)
    )
    names: list[str] = field(default_factory=list)

    def f(self, state_type):
        current_state_by_type = self.state[state_type]
        return current_state_by_type
