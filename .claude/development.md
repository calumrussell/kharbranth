# Git Workflow and PR Guidelines for Kharbranth

## Branch Structure

### Main Branch
- `main`: Primary development branch
- All PRs should target `main`
- Protected branch requiring PR reviews

### Feature Branches
- Create feature branches from `main`
- Use descriptive names: `fix-wsmanager-mut-self`, `add-connection-pooling`
- Keep branches focused on single features/fixes
- Should be tagged to an issue.

## Commit Guidelines

### Commit Message Format
Follow the existing pattern seen in recent commits:
- Use imperative mood: "Fix WSManager::write() requiring &mut self unnecessarily"
- Be descriptive about the change
- Should be used to divide up a change into self-contained parts.

## Pull Request Process

### Before Creating PR
1. Ensure all tests pass: `cargo test`
2. Check code compiles: `cargo build`
3. Run clippy for linting: `cargo clippy`
4. Format code: `cargo fmt`

### PR Creation
1. Create descriptive PR title
2. Include summary of changes
3. Reference related issues
4. Add test plan if applicable

### Common Issue Patterns to Look For

#### Connection Management Issues
- Memory leaks in connection pooling
- Improper cleanup of WebSocket connections
- Race conditions in concurrent connection handling

#### Error Handling
- Unwrap() calls that should use proper error handling
- Missing error propagation in async contexts
- Inconsistent error types across modules

#### Performance Issues
- Unnecessary cloning of large data structures
- Inefficient HashMap operations
- Blocking operations in async contexts

#### Code Quality
- Missing documentation for public APIs
- Inconsistent naming conventions
- Dead code or unused imports

## Testing Strategy

### Unit Tests
- Mock WebSocket connections using `tokio-test`
- Test error conditions and edge cases
- Verify connection lifecycle management

### Integration Tests
- Test full WebSocket connection flow
- Verify reconnection logic
- Test concurrent connection handling

## Code Review Checklist

- [ ] Code compiles without warnings
- [ ] All tests pass
- [ ] No clippy warnings
- [ ] Proper error handling
- [ ] Memory safety considerations
- [ ] Performance implications reviewed
- [ ] Documentation updated if needed
- [ ] Make sure PR does not contain unneeded files
