"""{backend_name} backend — delegates to Python module."""

from __future__ import annotations

from agent_sec_cli.security_middleware.result import ActionResult


class {BackendName}Backend:
    """Backend for {backend_name} — uses Python module."""

    def execute(self, ctx, **kwargs) -> ActionResult:
        """Execute the backend logic.

        Args:
            ctx: Request context (unused beyond tracing).
            **kwargs: Backend-specific parameters passed from CLI.

        Returns:
            ActionResult with success status, output, and exit code.
        """
        try:
            module = self._import_module()
            return self._run(module, **kwargs)
        except Exception as exc:
            return ActionResult(
                success=False,
                error=f"{backend_name} error: {exc}",
                exit_code=1,
            )

    @staticmethod
    def _import_module():
        """Lazily import the Python module."""
        import importlib

        mod = importlib.import_module("{module_path}")
        return mod

    @staticmethod
    def _run(module, **kwargs) -> ActionResult:
        """Execute the module logic.

        This method should be customized to call the appropriate
        function from the imported module (e.g., module.main(),
        module.verify(), etc.) and return an ActionResult.

        Example:
            result = module.verify(**kwargs)
            return ActionResult(
                success=result.success,
                stdout=result.output,
                data=result.data,
                exit_code=0 if result.success else 1,
            )
        """
        # TODO: Implement module-specific logic
        # Call module.main() or module.verify() etc.
        # Return ActionResult with success, stdout, data, exit_code
        raise NotImplementedError("Backend logic not implemented")
