import yaml

CONFIG_GROUP_NAME = "aws_ecs_executor"

with open("f") as config:
    options = yaml.safe_load(config)["config"][CONFIG_GROUP_NAME]["options"]
    file_defaults = {
        option: default for (option, value) in options.items() if (default := value.get("default"))
    }
