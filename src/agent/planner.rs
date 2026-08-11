//! Task planner — breaks down complex tasks into manageable steps.
//!
//! The planner analyzes the user's request and creates a structured plan.
//! For simple tasks, this may be a single step; for complex ones,
//! it creates a dependency-ordered list of sub-tasks.

/// A single step in an execution plan.
#[derive(Debug, Clone)]
pub struct PlanStep {
    /// Step number (1-indexed)
    pub number: usize,
    /// Description of what this step accomplishes
    pub description: String,
    /// Dependencies on other step numbers (0 means no dependency)
    pub depends_on: Vec<usize>,
    /// Current status
    pub status: StepStatus,
}

/// Status of a plan step.
#[derive(Debug, Clone, PartialEq)]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Failed(String),
    Skipped,
}

/// A complete task execution plan.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The original task description
    pub task: String,
    /// Ordered list of steps
    pub steps: Vec<PlanStep>,
    /// Overall plan status
    pub status: PlanStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanStatus {
    Draft,
    Executing,
    Completed,
    Failed,
}

/// The planner is responsible for creating and managing task plans.
#[derive(Debug)]
pub struct Planner {
    max_turns: u32,
}

impl Planner {
    /// Create a new planner.
    pub fn new(max_turns: u32) -> Self {
        Self { max_turns }
    }

    /// Create a simple plan for a task.
    ///
    /// For now, this creates a single-step plan. In the future, this will
    /// use the LLM to decompose complex tasks into sub-steps.
    pub fn create_plan(&self, task: &str) -> Plan {
        Plan {
            task: task.to_string(),
            steps: vec![PlanStep {
                number: 1,
                description: format!("Execute: {}", task),
                depends_on: vec![],
                status: StepStatus::Pending,
            }],
            status: PlanStatus::Draft,
        }
    }

    /// Get the next pending step that is ready to execute.
    pub fn next_step<'a>(&self, plan: &'a Plan) -> Option<&'a PlanStep> {
        plan.steps
            .iter()
            .find(|s| s.status == StepStatus::Pending && self.dependencies_met(s, plan))
    }

    /// Check if all dependencies for a step are satisfied.
    fn dependencies_met(&self, step: &PlanStep, plan: &Plan) -> bool {
        step.depends_on.iter().all(|dep_num| {
            plan.steps
                .iter()
                .find(|s| s.number == *dep_num)
                .map(|s| s.status == StepStatus::Completed)
                .unwrap_or(true)
        })
    }

    /// Get progress as a percentage.
    pub fn progress(&self, plan: &Plan) -> f32 {
        if plan.steps.is_empty() {
            return 100.0;
        }
        let completed = plan
            .steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Completed | StepStatus::Skipped))
            .count();
        (completed as f32 / plan.steps.len() as f32) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_plan() {
        let planner = Planner::new(50);
        let plan = planner.create_plan("Fix the login bug");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].number, 1);
        assert_eq!(plan.status, PlanStatus::Draft);
    }

    #[test]
    fn test_progress() {
        let planner = Planner::new(50);
        let mut plan = planner.create_plan("Test task");
        assert_eq!(planner.progress(&plan), 0.0);

        plan.steps[0].status = StepStatus::Completed;
        assert_eq!(planner.progress(&plan), 100.0);
    }
}

    #[test]
    fn test_next_step_respects_dependency_order() {
        let planner = Planner::new(50);
        let mut plan = planner.create_plan("task");
        plan.steps = vec![
            PlanStep { number: 1, description: "step 1".into(), depends_on: vec![], status: StepStatus::Pending },
            PlanStep { number: 2, description: "step 2".into(), depends_on: vec![1], status: StepStatus::Pending },
            PlanStep { number: 3, description: "step 3".into(), depends_on: vec![2], status: StepStatus::Pending },
        ];

        // Step 1 has no dependencies, so it comes first.
        assert_eq!(planner.next_step(&plan).unwrap().number, 1);

        plan.steps[0].status = StepStatus::Completed;
        // Step 2's dependency is now satisfied.
        assert_eq!(planner.next_step(&plan).unwrap().number, 2);

        plan.steps[1].status = StepStatus::Completed;
        assert_eq!(planner.next_step(&plan).unwrap().number, 3);

        plan.steps[2].status = StepStatus::Completed;
        assert!(planner.next_step(&plan).is_none());
    }

    #[test]
    fn test_next_step_skips_pending_dependencies() {
        let planner = Planner::new(50);
        let mut plan = planner.create_plan("task");
        plan.steps = vec![
            PlanStep { number: 1, description: "s1".into(), depends_on: vec![], status: StepStatus::Completed },
            PlanStep { number: 2, description: "s2".into(), depends_on: vec![1], status: StepStatus::Pending },
            PlanStep { number: 3, description: "s3".into(), depends_on: vec![2], status: StepStatus::Pending },
        ];

        // Step 3 depends on step 2 which is still pending, so step 2 is next.
        assert_eq!(planner.next_step(&plan).unwrap().number, 2);

        plan.steps[1].status = StepStatus::Completed;
        assert_eq!(planner.next_step(&plan).unwrap().number, 3);
    }

    #[test]
    fn test_next_step_none_when_all_blocked() {
        let planner = Planner::new(50);
        let mut plan = planner.create_plan("task");
        plan.steps = vec![
            PlanStep { number: 1, description: "s1".into(), depends_on: vec![], status: StepStatus::InProgress },
            PlanStep { number: 2, description: "s2".into(), depends_on: vec![1], status: StepStatus::Pending },
        ];

        // Step 1 is in progress (not pending) and step 2 is blocked on it.
        assert!(planner.next_step(&plan).is_none());
    }

    #[test]
    fn test_next_step_missing_dependency_treated_as_met() {
        let planner = Planner::new(50);
        let mut plan = planner.create_plan("task");
        plan.steps = vec![PlanStep {
            number: 1,
            description: "s1".into(),
            depends_on: vec![99], // no such step
            status: StepStatus::Pending,
        }];

        assert_eq!(planner.next_step(&plan).unwrap().number, 1);
    }

    #[test]
    fn test_progress_with_failures_and_empty_plan() {
        let planner = Planner::new(50);
        // Empty plans report 100% to avoid a divide-by-zero.
        let mut plan = planner.create_plan("x");
        plan.steps.clear();
        assert_eq!(planner.progress(&plan), 100.0);

        let mut plan = planner.create_plan("x");
        plan.steps[0].status = StepStatus::Failed("boom".into());
        assert_eq!(planner.progress(&plan), 0.0);

        plan.steps[0].status = StepStatus::Skipped;
        assert_eq!(planner.progress(&plan), 100.0);
    }
