try:
    from dotenv import load_dotenv
    load_dotenv()  # automatically reads .env if python-dotenv is installed
except ImportError:
    # python-dotenv is optional; the Rust host also passes YT_API_KEY per request.
    pass

__all__ = ["__version__"]
__version__ = "0.1.0"
