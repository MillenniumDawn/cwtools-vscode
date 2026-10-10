const RULES_CONFIG_SUMMARY = /^CWTools: \d+ rules-config error\(s\), first:/;

export function isRulesConfigSummaryNotification(message: string): boolean {
	return RULES_CONFIG_SUMMARY.test(message);
}
