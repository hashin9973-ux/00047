import os
import sys

# 00047 passes block fields as FIELD_<KEY> environment variables.
path = os.environ.get("FIELD_PATH", "")
sys.exit(0 if path and os.path.exists(path) else 1)
