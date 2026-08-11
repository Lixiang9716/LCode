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
