try:
    from dotenv import load_dotenv
    # Finds the repo's .env by walking up from this package (YT_API_KEY for search).
    load_dotenv()
except ImportError:
    # python-dotenv is optional; the service also passes api_key per request.
    pass

__all__ = ["__version__"]
__version__ = "0.1.0"
