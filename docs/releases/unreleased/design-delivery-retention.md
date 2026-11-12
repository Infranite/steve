Task deliverables outside Git now live in a persistent, private per-item `outputs` directory and remain available after worktree cleanup or a Node disconnects. Design and report delivery text is saved as a file there; the database holds its path rather than the full body. Daily file backups include these outputs. Multi-select delivery tags describe plans, code, reports, content, data and operations separately from the task type.

On the first start after upgrading, Steve repairs older delivery references when the saved full text is still available. It makes a verified database backup before changing those references; deleted files cannot be reconstructed.

When a task asks for a design without already asking for implementation, or explicitly calls for user confirmation first, Steve waits for the user to choose implementation, keep the design only or request changes. Automatic YOLO review cannot make this decision.
