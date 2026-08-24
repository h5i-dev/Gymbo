"""Make `import gymbo` work when running the tests from a source checkout
without installing the package (pytest adds the rootdir's conftest dir to
sys.path). An installed package takes precedence and this is a harmless no-op."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "src"))
