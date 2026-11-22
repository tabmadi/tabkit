package workflows

func flow(ctx C) {
	workflow.ExecuteActivity(ctx, "Unknown", 1)
}
