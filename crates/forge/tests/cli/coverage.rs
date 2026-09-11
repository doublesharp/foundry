use clap::CommandFactory;
use forge::cmd::coverage::CoverageArgs;
use foundry_common::fs::{self, files_with_ext};
use foundry_config::{CompilationRestrictions, SettingsOverrides};
use foundry_test_utils::{
    TestCommand, TestProject,
    snapbox::{Data, IntoData},
    util::OutputExt,
};
use serde_json::Value;
use std::path::Path;

#[track_caller]
fn assert_lcov(cmd: &mut TestCommand, data: impl IntoData) {
    cmd.args(["--report=lcov", "--report-file"]).assert_file(data.into_data());
}

fn basic_base(prj: TestProject, mut cmd: TestCommand) {
    cmd.args(["coverage", "--report=lcov", "--report=summary"]).assert_success().stdout_eq(str![[
        r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 2 tests for test/Counter.t.sol:CounterTest
[PASS] testFuzz_SetNumber(uint256) (runs: 256, [AVG_GAS])
[PASS] test_Increment() ([GAS])
Suite result: ok. 2 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 2 tests passed, 0 failed, 0 skipped (2 total tests)
Wrote LCOV report.

╭----------------------+---------------+---------------+------------+---------------╮
| File                 | % Lines       | % Statements  | % Branches | % Funcs       |
+===================================================================================+
| script/Counter.s.sol | 0.00% (0/5)   | 0.00% (0/3)   | N/A (0/0)  | 0.00% (0/2)   |
|----------------------+---------------+---------------+------------+---------------|
| src/Counter.sol      | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
|----------------------+---------------+---------------+------------+---------------|
| Total                | 44.44% (4/9)  | 40.00% (2/5)  | N/A (0/0)  | 50.00% (2/4)  |
╰----------------------+---------------+---------------+------------+---------------╯

"#
    ]]);

    let lcov = prj.root().join("lcov.info");
    assert!(lcov.exists(), "lcov.info was not created");
    let default_lcov = str![[r#"
TN:
SF:script/Counter.s.sol
DA:10,0
FN:10,CounterScript.setUp
FNDA:0,CounterScript.setUp
DA:12,0
FN:12,CounterScript.run
FNDA:0,CounterScript.run
DA:13,0
DA:15,0
DA:17,0
FNF:2
FNH:0
LF:5
LH:0
BRF:0
BRH:0
end_of_record
TN:
SF:src/Counter.sol
DA:7,258
FN:7,Counter.setNumber
FNDA:258,Counter.setNumber
DA:8,258
DA:11,1
FN:11,Counter.increment
FNDA:1,Counter.increment
DA:12,1
FNF:2
FNH:2
LF:4
LH:4
BRF:0
BRH:0
end_of_record

"#]];
    assert_data_eq!(Data::read_from(&lcov, None), default_lcov.clone());
    assert_lcov(
        cmd.forge_fuse().args(["coverage", "--report=lcov", "--lcov-version=1"]),
        default_lcov,
    );

    assert_lcov(
        cmd.forge_fuse().args(["coverage", "--report=lcov", "--lcov-version=2"]),
        str![[r#"
TN:
SF:script/Counter.s.sol
DA:10,0
FN:10,10,CounterScript.setUp
FNDA:0,CounterScript.setUp
DA:12,0
FN:12,18,CounterScript.run
FNDA:0,CounterScript.run
DA:13,0
DA:15,0
DA:17,0
FNF:2
FNH:0
LF:5
LH:0
BRF:0
BRH:0
end_of_record
TN:
SF:src/Counter.sol
DA:7,258
FN:7,9,Counter.setNumber
FNDA:258,Counter.setNumber
DA:8,258
DA:11,1
FN:11,13,Counter.increment
FNDA:1,Counter.increment
DA:12,1
FNF:2
FNH:2
LF:4
LH:4
BRF:0
BRH:0
end_of_record

"#]],
    );

    assert_lcov(
        cmd.forge_fuse().args(["coverage", "--report=lcov", "--lcov-version=2.2"]),
        str![[r#"
TN:
SF:script/Counter.s.sol
DA:10,0
FNL:0,10,10
FNA:0,0,CounterScript.setUp
DA:12,0
FNL:1,12,18
FNA:1,0,CounterScript.run
DA:13,0
DA:15,0
DA:17,0
FNF:2
FNH:0
LF:5
LH:0
BRF:0
BRH:0
end_of_record
TN:
SF:src/Counter.sol
DA:7,258
FNL:2,7,9
FNA:2,258,Counter.setNumber
DA:8,258
DA:11,1
FNL:3,11,13
FNA:3,1,Counter.increment
DA:12,1
FNF:2
FNH:2
LF:4
LH:4
BRF:0
BRH:0
end_of_record

"#]],
    );
}

forgetest_init!(basic, |prj, cmd| {
    prj.initialize_default_contracts();
    basic_base(prj, cmd);
});

forgetest_init!(basic_crlf, |prj, cmd| {
    prj.initialize_default_contracts();
    // Manually replace `\n` with `\r\n` in the source file.
    let make_crlf = |path: &Path| {
        fs::write(path, fs::read_to_string(path).unwrap().replace('\n', "\r\n")).unwrap()
    };
    make_crlf(&prj.paths().sources.join("Counter.sol"));
    make_crlf(&prj.paths().scripts.join("Counter.s.sol"));

    // Should have identical stdout and lcov output.
    basic_base(prj, cmd);
});

forgetest!(setup, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    int public i;

    function init() public {
        i = 0;
    }

    function foo() public {
        i = 1;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a;

    function setUp() public {
        a = new AContract();
        a.init();
    }

    function testFoo() public {
        a.foo();
    }
}
    "#,
    );

    // Assert 100% coverage (init function coverage called in setUp is accounted).
    cmd.arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+------------+---------------╮
| File              | % Lines       | % Statements  | % Branches | % Funcs       |
+================================================================================+
| src/AContract.sol | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
|-------------------+---------------+---------------+------------+---------------|
| Total             | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
╰-------------------+---------------+---------------+------------+---------------╯

"#]]);
});

forgetest!(setup_md, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    int public i;

    function init() public {
        i = 0;
    }

    function foo() public {
        i = 1;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a;

    function setUp() public {
        a = new AContract();
        a.init();
    }

    function testFoo() public {
        a.foo();
    }
}
    "#,
    );

    // Assert 100% coverage (init function coverage called in setUp is accounted).
    cmd.arg("coverage").args(["--md"]).assert_success().stdout_eq(str![[r#"
...
| File              | % Lines       | % Statements  | % Branches | % Funcs       |
|-------------------|---------------|---------------|------------|---------------|
| src/AContract.sol | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
| Total             | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |

"#]]);
});

forgetest!(no_match, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    int public i;

    function init() public {
        i = 0;
    }

    function foo() public {
        i = 1;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a;

    function setUp() public {
        a = new AContract();
        a.init();
    }

    function testFoo() public {
        a.foo();
    }
}
    "#,
    );

    prj.add_source(
        "BContract.sol",
        r#"
contract BContract {
    int public i;

    function init() public {
        i = 0;
    }

    function foo() public {
        i = 1;
    }
}
    "#,
    );

    prj.add_source(
        "BContractTest.sol",
        r#"
import "./test.sol";
import {BContract} from "./BContract.sol";

contract BContractTest is DSTest {
    BContract a;

    function setUp() public {
        a = new BContract();
        a.init();
    }

    function testFoo() public {
        a.foo();
    }
}
    "#,
    );

    // Assert AContract is not included in report.
    cmd.arg("coverage").arg("--no-match-coverage=AContract").assert_success().stdout_eq(str![[
        r#"
...
╭-------------------+---------------+---------------+------------+---------------╮
| File              | % Lines       | % Statements  | % Branches | % Funcs       |
+================================================================================+
| src/BContract.sol | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
|-------------------+---------------+---------------+------------+---------------|
| Total             | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
╰-------------------+---------------+---------------+------------+---------------╯

"#
    ]]);
});

// `[profile.default.coverage] skip_files` should exclude matching sources from
// the coverage report just like `--no-match-coverage`, but using glob patterns
// from `foundry.toml`.
forgetest!(skip_files_via_config, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    int public i;

    function init() public {
        i = 0;
    }

    function foo() public {
        i = 1;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a;

    function setUp() public {
        a = new AContract();
        a.init();
    }

    function testFoo() public {
        a.foo();
    }
}
    "#,
    );

    prj.add_source(
        "BContract.sol",
        r#"
contract BContract {
    int public i;

    function init() public {
        i = 0;
    }

    function foo() public {
        i = 1;
    }
}
    "#,
    );

    prj.add_source(
        "BContractTest.sol",
        r#"
import "./test.sol";
import {BContract} from "./BContract.sol";

contract BContractTest is DSTest {
    BContract a;

    function setUp() public {
        a = new BContract();
        a.init();
    }

    function testFoo() public {
        a.foo();
    }
}
    "#,
    );

    prj.update_config(|config| {
        config.coverage.skip_files =
            vec!["src/AContract.sol".into(), "src/AContractTest.sol".into()];
    });

    // Assert AContract.sol is excluded via the config glob; BContract.sol still
    // appears in the report.
    cmd.arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+------------+---------------╮
| File              | % Lines       | % Statements  | % Branches | % Funcs       |
+================================================================================+
| src/BContract.sol | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
|-------------------+---------------+---------------+------------+---------------|
| Total             | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
╰-------------------+---------------+---------------+------------+---------------╯

"#]]);
});

forgetest!(assert, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    function checkA(uint256 a) external pure returns (bool) {
        assert(a > 2);
        return true;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

interface Vm {
    function expectRevert() external;
}

contract AContractTest is DSTest {
    Vm constant vm = Vm(HEVM_ADDRESS);
    AContract a = new AContract();

    function testAssertBranch() external {
        bool result = a.checkA(10);
        assertTrue(result);
    }

    function testAssertRevertBranch() external {
        vm.expectRevert();
        a.checkA(1);
    }
}
    "#,
    );

    // Assert 50% statement coverage for assert failure (assert not considered a branch).
    cmd.arg("coverage").args(["--mt", "testAssertRevertBranch"]).assert_success().stdout_eq(str![
        [r#"
...
╭-------------------+--------------+--------------+------------+---------------╮
| File              | % Lines      | % Statements | % Branches | % Funcs       |
+==============================================================================+
| src/AContract.sol | 66.67% (2/3) | 50.00% (1/2) | N/A (0/0)  | 100.00% (1/1) |
|-------------------+--------------+--------------+------------+---------------|
| Total             | 66.67% (2/3) | 50.00% (1/2) | N/A (0/0)  | 100.00% (1/1) |
╰-------------------+--------------+--------------+------------+---------------╯

"#]
    ]);

    // Assert 100% statement coverage for proper assert (assert not considered a branch).
    cmd.forge_fuse().arg("coverage").args(["--mt", "testAssertBranch"]).assert_success().stdout_eq(
        str![[r#"
...
╭-------------------+---------------+---------------+------------+---------------╮
| File              | % Lines       | % Statements  | % Branches | % Funcs       |
+================================================================================+
| src/AContract.sol | 100.00% (3/3) | 100.00% (2/2) | N/A (0/0)  | 100.00% (1/1) |
|-------------------+---------------+---------------+------------+---------------|
| Total             | 100.00% (3/3) | 100.00% (2/2) | N/A (0/0)  | 100.00% (1/1) |
╰-------------------+---------------+---------------+------------+---------------╯

"#]],
    );
});

forgetest!(require, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    function checkRequire(bool doNotRevert) public view {
        require(doNotRevert, "reverted");
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

interface Vm {
    function expectRevert(bytes calldata revertData) external;
}

contract AContractTest is DSTest {
    Vm constant vm = Vm(HEVM_ADDRESS);
    AContract a = new AContract();

    function testRequireRevert() external {
        vm.expectRevert(abi.encodePacked("reverted"));
        a.checkRequire(false);
    }

    function testRequireNoRevert() external {
        a.checkRequire(true);
    }
}
    "#,
    );

    // Assert 50% branch coverage if only revert tested.
    cmd.arg("coverage").args(["--mt", "testRequireRevert"]).assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+--------------+---------------╮
| File              | % Lines       | % Statements  | % Branches   | % Funcs       |
+==================================================================================+
| src/AContract.sol | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
|-------------------+---------------+---------------+--------------+---------------|
| Total             | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
╰-------------------+---------------+---------------+--------------+---------------╯

"#]]);

    // Assert 50% branch coverage if only happy path tested.
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--mt", "testRequireNoRevert"])
        .assert_success()
        .stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+--------------+---------------╮
| File              | % Lines       | % Statements  | % Branches   | % Funcs       |
+==================================================================================+
| src/AContract.sol | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
|-------------------+---------------+---------------+--------------+---------------|
| Total             | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
╰-------------------+---------------+---------------+--------------+---------------╯

"#]]);

    // Assert 100% branch coverage.
    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+---------------+---------------╮
| File              | % Lines       | % Statements  | % Branches    | % Funcs       |
+===================================================================================+
| src/AContract.sol | 100.00% (2/2) | 100.00% (1/1) | 100.00% (2/2) | 100.00% (1/1) |
|-------------------+---------------+---------------+---------------+---------------|
| Total             | 100.00% (2/2) | 100.00% (1/1) | 100.00% (2/2) | 100.00% (1/1) |
╰-------------------+---------------+---------------+---------------+---------------╯

"#]]);
});

// Smoke test mixing several constructs in one contract under `--ir-minimum` (viaIR):
// a `require` branch, inline assembly counted as a statement, a ternary, and a logical OR.
// The six branch outcomes (require true/false, ternary true/false, OR left/right) are all
// exercised, demonstrating accurate branch coverage with the optimizer/IR pipeline on.
forgetest!(instrumented_mixed_constructs, |prj, cmd| {
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    function guarded(uint256 value) external pure returns (uint256) {
        require(value > 1, "small");
        return value;
    }

    function addFive(uint256 value) external pure returns (uint256 result) {
        assembly {
            result := add(value, 5)
        }
    }

    function choose(bool flag) external pure returns (uint256) {
        return flag ? 1 : 2;
    }

    function either(bool left, bool right) external pure returns (bool) {
        return left || right;
    }
}
    "#,
    );

    prj.add_test(
        "AContractTest.sol",
        r#"
import {AContract} from "../src/AContract.sol";

interface Vm {
    function expectRevert(bytes calldata revertData) external;
}

contract AContractTest {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    AContract a = new AContract();

    function testRequireHappyPath() external {
        require(a.guarded(2) == 2);
    }

    function testRequireRevertPath() external {
        vm.expectRevert(bytes("small"));
        a.guarded(1);
    }

    function testAssemblyStatement() external {
        require(a.addFive(7) == 12);
    }

    function testTernaryBothPaths() external {
        require(a.choose(true) == 1);
        require(a.choose(false) == 2);
    }

    function testLogicalOrBothPaths() external {
        require(a.either(true, false));
        require(a.either(false, true));
    }
}
    "#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--ir-minimum", "--exclude-tests"])
        .assert_success()
        .stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+---------------+---------------╮
| File              | % Lines       | % Statements  | % Branches    | % Funcs       |
+===================================================================================+
| src/AContract.sol | 100.00% (9/9) | 100.00% (5/5) | 100.00% (6/6) | 100.00% (4/4) |
|-------------------+---------------+---------------+---------------+---------------|
| Total             | 100.00% (9/9) | 100.00% (5/5) | 100.00% (6/6) | 100.00% (4/4) |
╰-------------------+---------------+---------------+---------------+---------------╯

"#]]);
});

forgetest!(line_hit_not_doubled, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    int public i;

    function foo() public {
        i = 1;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a = new AContract();

    function testFoo() public {
        a.foo();
    }
}
    "#,
    );

    // We want to make sure DA:8,1 is added only once so line hit is not doubled.
    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/AContract.sol
DA:7,1
FN:7,AContract.foo
FNDA:1,AContract.foo
DA:8,1
FNF:1
FNH:1
LF:2
LH:2
BRF:0
BRH:0
end_of_record

"#]],
    );
});

forgetest!(branch, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Foo.sol",
        r#"
contract Foo {
    error Gte1(uint256 number, uint256 firstElement);

    enum Status {
        NULL,
        OPEN,
        CLOSED
    }

    struct Item {
        Status status;
        uint256 value;
    }

    mapping(uint256 => Item) internal items;
    uint256 public nextId = 1;

    function getItem(uint256 id) public view returns (Item memory item) {
        item = items[id];
    }

    function addItem(uint256 value) public returns (uint256 id) {
        id = nextId;
        items[id] = Item(Status.OPEN, value);
        nextId++;
    }

    function closeIfEqValue(uint256 id, uint256 value) public {
        if (items[id].value == value) {
            items[id].status = Status.CLOSED;
        }
    }

    function incrementIfEqValue(uint256 id, uint256 value) public {
        if (items[id].value == value) {
            items[id].value = value + 1;
        }
    }

    function foo(uint256 a) external pure {
        if (a < 10) {
            if (a < 3) {
                assert(a == 1);
            } else {
                assert(a == 5);
            }
        } else {
            assert(a == 60);
        }
    }

    function countOdd(uint256[] memory arr) external pure returns (uint256 count) {
        uint256 length = arr.length;
        for (uint256 i = 0; i < length; ++i) {
            if (arr[i] % 2 == 1) {
                count++;
                arr[0];
            }
        }
    }

    function checkLt(uint256 number, uint256[] memory arr) external pure returns (bool) {
        if (number >= arr[0]) {
            revert Gte1(number, arr[0]);
        }
        return true;
    }

    function checkEmptyStatements(uint256 number, uint256[] memory arr) external pure returns (bool) {
        // Check that empty statements are covered.
        if (number >= arr[0]) {
            // Do nothing
        } else {
            // Do nothing.
        }
        if (number >= arr[0]) {}

        return true;
    }

    function singlePathCoverage(uint256 number) external pure {
        if (number < 10) {
            if (number < 5) {
                number++;
            }
            number++;
        }
    }
}
    "#,
    );

    prj.add_source(
        "FooTest.sol",
        r#"
import "./test.sol";
import {Foo} from "./Foo.sol";

interface Vm {
    function expectRevert(bytes calldata revertData) external;
    function expectRevert() external;
}

contract FooTest is DSTest {
    Vm constant vm = Vm(HEVM_ADDRESS);
    Foo internal foo = new Foo();

    function test_issue_7784() external {
        foo.foo(1);
        vm.expectRevert();
        foo.foo(2);
        vm.expectRevert();
        foo.foo(4);
        foo.foo(5);
        foo.foo(60);
        vm.expectRevert();
        foo.foo(70);
    }

    function test_issue_4310() external {
        uint256[] memory arr = new uint256[](3);
        arr[0] = 78;
        arr[1] = 493;
        arr[2] = 700;
        uint256 count = foo.countOdd(arr);
        assertEq(count, 1);

        arr = new uint256[](4);
        arr[0] = 78;
        arr[1] = 493;
        arr[2] = 700;
        arr[3] = 1729;
        count = foo.countOdd(arr);
        assertEq(count, 2);
    }

    function test_issue_4315() external {
        uint256 value = 42;
        uint256 id = foo.addItem(value);
        assertEq(id, 1);
        assertEq(foo.nextId(), 2);
        Foo.Item memory item = foo.getItem(id);
        assertEq(uint8(item.status), uint8(Foo.Status.OPEN));
        assertEq(item.value, value);

        foo = new Foo();
        id = foo.addItem(value);
        foo.closeIfEqValue(id, 903);
        item = foo.getItem(id);
        assertEq(uint8(item.status), uint8(Foo.Status.OPEN));

        foo = new Foo();
        foo.addItem(value);
        foo.closeIfEqValue(id, 42);
        item = foo.getItem(id);
        assertEq(uint8(item.status), uint8(Foo.Status.CLOSED));

        foo = new Foo();
        id = foo.addItem(value);
        foo.incrementIfEqValue(id, 903);
        item = foo.getItem(id);
        assertEq(item.value, 42);

        foo = new Foo();
        id = foo.addItem(value);
        foo.incrementIfEqValue(id, 42);
        item = foo.getItem(id);
        assertEq(item.value, 43);
    }

    function test_issue_4309() external {
        uint256[] memory arr = new uint256[](1);
        arr[0] = 1;
        uint256 number = 2;
        vm.expectRevert(abi.encodeWithSelector(Foo.Gte1.selector, number, arr[0]));
        foo.checkLt(number, arr);

        number = 1;
        vm.expectRevert(abi.encodeWithSelector(Foo.Gte1.selector, number, arr[0]));
        foo.checkLt(number, arr);

        number = 0;
        bool result = foo.checkLt(number, arr);
        assertTrue(result);
    }

    function test_issue_4314() external {
        uint256[] memory arr = new uint256[](1);
        arr[0] = 1;
        foo.checkEmptyStatements(0, arr);
    }

    function test_single_path_child_branch() external {
        foo.singlePathCoverage(1);
    }

    function test_single_path_parent_branch() external {
        foo.singlePathCoverage(9);
    }

    function test_single_path_branch() external {
        foo.singlePathCoverage(15);
    }
}
    "#,
    );

    // Assert no coverage for single path branch. 2 branches (parent and child) not covered.
    cmd.arg("coverage")
        .args(["--nmt", "test_single_path_child_branch|test_single_path_parent_branch"])
        .assert_success()
        .stdout_eq(str![[r#"
...
╭-------------+----------------+----------------+---------------+---------------╮
| File        | % Lines        | % Statements   | % Branches    | % Funcs       |
+===============================================================================+
| src/Foo.sol | 91.67% (33/36) | 90.00% (27/30) | 80.00% (8/10) | 100.00% (9/9) |
|-------------+----------------+----------------+---------------+---------------|
| Total       | 91.67% (33/36) | 90.00% (27/30) | 80.00% (8/10) | 100.00% (9/9) |
╰-------------+----------------+----------------+---------------+---------------╯

"#]]);

    // Assert no coverage for single path child branch. 1 branch (child) not covered.
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--nmt", "test_single_path_child_branch"])
        .assert_success()
        .stdout_eq(str![[r#"
...
╭-------------+----------------+----------------+---------------+---------------╮
| File        | % Lines        | % Statements   | % Branches    | % Funcs       |
+===============================================================================+
| src/Foo.sol | 97.22% (35/36) | 96.67% (29/30) | 90.00% (9/10) | 100.00% (9/9) |
|-------------+----------------+----------------+---------------+---------------|
| Total       | 97.22% (35/36) | 96.67% (29/30) | 90.00% (9/10) | 100.00% (9/9) |
╰-------------+----------------+----------------+---------------+---------------╯

"#]]);

    // Assert 100% coverage.
    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------+-----------------+-----------------+-----------------+---------------╮
| File        | % Lines         | % Statements    | % Branches      | % Funcs       |
+===================================================================================+
| src/Foo.sol | 100.00% (36/36) | 100.00% (30/30) | 100.00% (10/10) | 100.00% (9/9) |
|-------------+-----------------+-----------------+-----------------+---------------|
| Total       | 100.00% (36/36) | 100.00% (30/30) | 100.00% (10/10) | 100.00% (9/9) |
╰-------------+-----------------+-----------------+-----------------+---------------╯

"#]]);
});

forgetest!(function_call, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    struct Custom {
        bool a;
        uint256 b;
    }

    function coverMe() external returns (bool) {
        // Next lines should not be counted in coverage.
        string("");
        uint256(1);
        address(this);
        bool(false);
        Custom(true, 10);
        // Next lines should be counted in coverage.
        uint256 a = uint256(1);
        Custom memory cust = Custom(false, 100);
        privateWithNoBody();
        privateWithBody();
        publicWithNoBody();
        publicWithBody();
        return true;
    }

    function privateWithNoBody() private {}

    function privateWithBody() private returns (bool) {
        return true;
    }

    function publicWithNoBody() private {}

    function publicWithBody() private returns (bool) {
        return true;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a = new AContract();

    function testTypeConversionCoverage() external {
        a.coverMe();
    }
}
    "#,
    );

    // Assert 100% coverage.
    cmd.arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+-----------------+---------------+------------+---------------╮
| File              | % Lines         | % Statements  | % Branches | % Funcs       |
+==================================================================================+
| src/AContract.sol | 100.00% (14/14) | 100.00% (9/9) | N/A (0/0)  | 100.00% (5/5) |
|-------------------+-----------------+---------------+------------+---------------|
| Total             | 100.00% (14/14) | 100.00% (9/9) | N/A (0/0)  | 100.00% (5/5) |
╰-------------------+-----------------+---------------+------------+---------------╯

"#]]);
});

forgetest!(try_catch, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Foo.sol",
        r#"
contract Foo {
    address public owner;

    constructor(address _owner) {
        require(_owner != address(0), "invalid address");
        assert(_owner != 0x0000000000000000000000000000000000000001);
        owner = _owner;
    }

    function myFunc(uint256 x) public pure returns (string memory) {
        require(x != 0, "require failed");
        return "my func was called";
    }
}

contract Bar {
    event Log(string message);
    event LogBytes(bytes data);

    Foo public foo;

    constructor() {
        foo = new Foo(msg.sender);
    }

    function tryCatchExternalCall(uint256 _i) public {
        try foo.myFunc(_i) returns (string memory result) {
            emit Log(result);
        } catch {
            emit Log("external call failed");
        }
    }

    function tryCatchNewContract(address _owner) public {
        try new Foo(_owner) returns (Foo foo_) {
            emit Log("Foo created");
        } catch Error(string memory reason) {
            emit Log(reason);
        } catch (bytes memory reason) {}
    }

    function tryCatchAllNewContract(address _owner) public {
        try new Foo(_owner) returns (Foo foo_) {} catch {}
    }
}
    "#,
    );

    prj.add_source(
        "FooTest.sol",
        r#"
import "./test.sol";
import {Bar, Foo} from "./Foo.sol";

interface Vm {
    function expectRevert() external;
}

contract FooTest is DSTest {
    Vm constant vm = Vm(HEVM_ADDRESS);

    function test_happy_foo_coverage() external {
        vm.expectRevert();
        Foo foo = new Foo(address(0));
        vm.expectRevert();
        foo = new Foo(address(1));
        foo = new Foo(address(2));
    }

    function test_happy_path_coverage() external {
        Bar bar = new Bar();
        bar.tryCatchNewContract(0x0000000000000000000000000000000000000002);
        bar.tryCatchAllNewContract(0x0000000000000000000000000000000000000002);
        bar.tryCatchExternalCall(1);
    }

    function test_coverage() external {
        Bar bar = new Bar();
        bar.tryCatchNewContract(0x0000000000000000000000000000000000000000);
        bar.tryCatchNewContract(0x0000000000000000000000000000000000000001);
        bar.tryCatchAllNewContract(0x0000000000000000000000000000000000000001);
        bar.tryCatchExternalCall(0);
    }
}
    "#,
    );

    // Assert coverage not 100% for happy paths only.
    cmd.arg("coverage").args(["--mt", "happy"]).assert_success().stdout_eq(str![[r#"
...
╭-------------+----------------+----------------+--------------+---------------╮
| File        | % Lines        | % Statements   | % Branches   | % Funcs       |
+==============================================================================+
| src/Foo.sol | 77.27% (17/22) | 78.57% (11/14) | 66.67% (6/9) | 100.00% (6/6) |
|-------------+----------------+----------------+--------------+---------------|
| Total       | 77.27% (17/22) | 78.57% (11/14) | 66.67% (6/9) | 100.00% (6/6) |
╰-------------+----------------+----------------+--------------+---------------╯

"#]]);

    // Assert 100% branch coverage (including clauses without body).
    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------+-----------------+-----------------+---------------+---------------╮
| File        | % Lines         | % Statements    | % Branches    | % Funcs       |
+=================================================================================+
| src/Foo.sol | 100.00% (22/22) | 100.00% (14/14) | 100.00% (9/9) | 100.00% (6/6) |
|-------------+-----------------+-----------------+---------------+---------------|
| Total       | 100.00% (22/22) | 100.00% (14/14) | 100.00% (9/9) | 100.00% (6/6) |
╰-------------+-----------------+-----------------+---------------+---------------╯

"#]]);
});

forgetest!(yul, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Foo.sol",
        r#"
contract Foo {
    uint256[] dynamicArray;

    function readDynamicArrayLength() public view returns (uint256 length) {
        assembly {
            length := sload(dynamicArray.slot)
        }
    }

    function switchAndIfStatements(uint256 n) public pure {
        uint256 y;
        assembly {
            switch n
            case 0 { y := 0 }
            case 1 { y := 1 }
            default { y := n }

            if y { y := 2 }
        }
    }

    function yulForLoop(uint256 n) public {
        uint256 y;
        assembly {
            for { let i := 0 } lt(i, n) { i := add(i, 1) } { y := add(y, 1) }

            let j := 0
            for {} lt(j, n) { j := add(j, 1) } { j := add(j, 2) }
        }
    }

    function hello() public pure returns (bool, uint256, bytes32) {
        bool x;
        uint256 y;
        bytes32 z;

        assembly {
            x := 1
            y := 0xa
            z := "Hello World!"
        }

        return (x, y, z);
    }

    function inlineFunction() public returns (uint256) {
        uint256 result;
        assembly {
            function sum(a, b) -> c {
                c := add(a, b)
            }

            function multiply(a, b) -> c {
                for { let i := 0 } lt(i, b) { i := add(i, 1) } { c := add(c, a) }
            }

            result := sum(2, 3)
            result := multiply(result, 5)
        }
        return result;
    }
}
    "#,
    );

    prj.add_source(
        "FooTest.sol",
        r#"
import "./test.sol";
import {Foo} from "./Foo.sol";

contract FooTest is DSTest {
    function test_foo_coverage() external {
        Foo foo = new Foo();
        foo.switchAndIfStatements(0);
        foo.switchAndIfStatements(1);
        foo.switchAndIfStatements(2);
        foo.yulForLoop(2);
        foo.hello();
        foo.readDynamicArrayLength();
        foo.inlineFunction();
    }
}
    "#,
    );

    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------+-----------------+-----------------+---------------+---------------╮
| File        | % Lines         | % Statements    | % Branches    | % Funcs       |
+=================================================================================+
| src/Foo.sol | 100.00% (30/30) | 100.00% (40/40) | 100.00% (1/1) | 100.00% (7/7) |
|-------------+-----------------+-----------------+---------------+---------------|
| Total       | 100.00% (30/30) | 100.00% (40/40) | 100.00% (1/1) | 100.00% (7/7) |
╰-------------+-----------------+-----------------+---------------+---------------╯

"#]]);
});

forgetest!(misc, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Foo.sol",
        r#"
struct Custom {
    int256 f1;
}

contract A {
    function f(Custom memory custom) public returns (int256) {
        return custom.f1;
    }
}

contract B {
    uint256 public x;

    constructor(uint256 a) payable {
        x = a;
    }
}

contract C {
    function create() public {
        B b = new B{value: 1}(2);
        b = new B{value: 1}(2);
        b = (new B){value: 1}(2);
    }
}

contract D {
    uint256 index;

    function g() public {
        (uint256 x,, uint256 y) = (7, true, 2);
        (x, y) = (y, x);
        (index,,) = (7, true, 2);
    }
}
    "#,
    );

    prj.add_source(
        "FooTest.sol",
        r#"
import "./test.sol";
import "./Foo.sol";

interface Vm {
    function deal(address account, uint256 newBalance) external;
}

contract FooTest is DSTest {
    Vm constant vm = Vm(HEVM_ADDRESS);

    function test_member_access_coverage() external {
        A a = new A();
        Custom memory cust = Custom(1);
        a.f(cust);
    }

    function test_new_expression_coverage() external {
        B b = new B(1);
        b.x();
        C c = new C();
        vm.deal(address(c), 100 ether);
        c.create();
    }

    function test_tuple_coverage() external {
        D d = new D();
        d.g();
    }
}
    "#,
    );

    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------+-----------------+---------------+------------+---------------╮
| File        | % Lines         | % Statements  | % Branches | % Funcs       |
+============================================================================+
| src/Foo.sol | 100.00% (12/12) | 100.00% (9/9) | N/A (0/0)  | 100.00% (4/4) |
|-------------+-----------------+---------------+------------+---------------|
| Total       | 100.00% (12/12) | 100.00% (9/9) | N/A (0/0)  | 100.00% (4/4) |
╰-------------+-----------------+---------------+------------+---------------╯

"#]]);
});

// https://github.com/foundry-rs/foundry/issues/8605
forgetest!(single_statement, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    event IsTrue(bool isTrue);
    event IsFalse(bool isFalse);

    function ifElseStatementIgnored(bool flag) external returns (bool) {
        if (flag) emit IsTrue(true);
        else emit IsFalse(false);

        bool flag2;
        if (flag) flag2 = true;
        else flag2 = false;

        if (flag2) return true;
        else return false;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a = new AContract();

    function testTrueCoverage() external {
        a.ifElseStatementIgnored(true);
    }

    function testFalseCoverage() external {
        a.ifElseStatementIgnored(false);
    }
}
    "#,
    );

    // Assert 50% coverage for true branches.
    cmd.arg("coverage").args(["--mt", "testTrueCoverage"]).assert_success().stdout_eq(str![[r#"
...
╭-------------------+--------------+--------------+--------------+---------------╮
| File              | % Lines      | % Statements | % Branches   | % Funcs       |
+================================================================================+
| src/AContract.sol | 62.50% (5/8) | 57.14% (4/7) | 50.00% (3/6) | 100.00% (1/1) |
|-------------------+--------------+--------------+--------------+---------------|
| Total             | 62.50% (5/8) | 57.14% (4/7) | 50.00% (3/6) | 100.00% (1/1) |
╰-------------------+--------------+--------------+--------------+---------------╯

"#]]);

    // Assert 50% coverage for false branches.
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--mt", "testFalseCoverage"])
        .assert_success()
        .stdout_eq(str![[r#"
...
╭-------------------+--------------+--------------+--------------+---------------╮
| File              | % Lines      | % Statements | % Branches   | % Funcs       |
+================================================================================+
| src/AContract.sol | 62.50% (5/8) | 57.14% (4/7) | 50.00% (3/6) | 100.00% (1/1) |
|-------------------+--------------+--------------+--------------+---------------|
| Total             | 62.50% (5/8) | 57.14% (4/7) | 50.00% (3/6) | 100.00% (1/1) |
╰-------------------+--------------+--------------+--------------+---------------╯

"#]]);

    // Assert 100% coverage (true/false branches properly covered).
    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+---------------+---------------╮
| File              | % Lines       | % Statements  | % Branches    | % Funcs       |
+===================================================================================+
| src/AContract.sol | 100.00% (8/8) | 100.00% (7/7) | 100.00% (6/6) | 100.00% (1/1) |
|-------------------+---------------+---------------+---------------+---------------|
| Total             | 100.00% (8/8) | 100.00% (7/7) | 100.00% (6/6) | 100.00% (1/1) |
╰-------------------+---------------+---------------+---------------+---------------╯

"#]]);
});

forgetest!(single_statement_loop, |prj, cmd| {
    // TODO(dani): the specific case of `if (x) continue/break` is not properly covered.
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    function ifBreakContinueIgnored(bool flag) external returns (uint256 sum) {
        for (uint256 i = 0; i < 5; i++) {
            if (flag) continue;
            sum += i;
        }

        for (uint256 i = 0; i < 5; i++) {
            if (flag) break;
            sum += i;
        }
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a = new AContract();

    function testTrueCoverage() external {
        a.ifBreakContinueIgnored(true);
    }

    function testFalseCoverage() external {
        a.ifBreakContinueIgnored(false);
    }
}
    "#,
    );

    // Assert 50% coverage for true branches.
    cmd.arg("coverage").args(["--mt", "testTrueCoverage"]).assert_success().stdout_eq(str![[r#"
...
╭-------------------+--------------+---------------+---------------+---------------╮
| File              | % Lines      | % Statements  | % Branches    | % Funcs       |
+==================================================================================+
| src/AContract.sol | 71.43% (5/7) | 70.00% (7/10) | 100.00% (2/2) | 100.00% (1/1) |
|-------------------+--------------+---------------+---------------+---------------|
| Total             | 71.43% (5/7) | 70.00% (7/10) | 100.00% (2/2) | 100.00% (1/1) |
╰-------------------+--------------+---------------+---------------+---------------╯

"#]]);

    // Assert 50% coverage for false branches.
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--mt", "testFalseCoverage"])
        .assert_success()
        .stdout_eq(str![[r#"
...
╭-------------------+---------------+-----------------+---------------+---------------╮
| File              | % Lines       | % Statements    | % Branches    | % Funcs       |
+=====================================================================================+
| src/AContract.sol | 100.00% (7/7) | 100.00% (10/10) | 100.00% (2/2) | 100.00% (1/1) |
|-------------------+---------------+-----------------+---------------+---------------|
| Total             | 100.00% (7/7) | 100.00% (10/10) | 100.00% (2/2) | 100.00% (1/1) |
╰-------------------+---------------+-----------------+---------------+---------------╯

"#]]);

    // Assert 100% coverage (true/false branches properly covered).
    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+-----------------+---------------+---------------╮
| File              | % Lines       | % Statements    | % Branches    | % Funcs       |
+=====================================================================================+
| src/AContract.sol | 100.00% (7/7) | 100.00% (10/10) | 100.00% (2/2) | 100.00% (1/1) |
|-------------------+---------------+-----------------+---------------+---------------|
| Total             | 100.00% (7/7) | 100.00% (10/10) | 100.00% (2/2) | 100.00% (1/1) |
╰-------------------+---------------+-----------------+---------------+---------------╯

"#]]);
});

// https://github.com/foundry-rs/foundry/issues/8604
forgetest!(branch_with_calldata_reads, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    event IsTrue(bool isTrue);
    event IsFalse(bool isFalse);

    function execute(bool[] calldata isTrue) external {
        for (uint256 i = 0; i < isTrue.length; i++) {
            if (isTrue[i]) {
                emit IsTrue(isTrue[i]);
            } else {
                emit IsFalse(!isTrue[i]);
            }
        }
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a = new AContract();

    function testTrueCoverage() external {
        bool[] memory isTrue = new bool[](1);
        isTrue[0] = true;
        a.execute(isTrue);
    }

    function testFalseCoverage() external {
        bool[] memory isFalse = new bool[](1);
        isFalse[0] = false;
        a.execute(isFalse);
    }
}
    "#,
    );

    // Assert 50% coverage for true branches.
    cmd.arg("coverage").args(["--mt", "testTrueCoverage"]).assert_success().stdout_eq(str![[r#"
...
╭-------------------+--------------+--------------+--------------+---------------╮
| File              | % Lines      | % Statements | % Branches   | % Funcs       |
+================================================================================+
| src/AContract.sol | 80.00% (4/5) | 80.00% (4/5) | 50.00% (1/2) | 100.00% (1/1) |
|-------------------+--------------+--------------+--------------+---------------|
| Total             | 80.00% (4/5) | 80.00% (4/5) | 50.00% (1/2) | 100.00% (1/1) |
╰-------------------+--------------+--------------+--------------+---------------╯

"#]]);

    // Assert 50% coverage for false branches.
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--mt", "testFalseCoverage"])
        .assert_success()
        .stdout_eq(str![[r#"
...
╭-------------------+--------------+--------------+--------------+---------------╮
| File              | % Lines      | % Statements | % Branches   | % Funcs       |
+================================================================================+
| src/AContract.sol | 60.00% (3/5) | 80.00% (4/5) | 50.00% (1/2) | 100.00% (1/1) |
|-------------------+--------------+--------------+--------------+---------------|
| Total             | 60.00% (3/5) | 80.00% (4/5) | 50.00% (1/2) | 100.00% (1/1) |
╰-------------------+--------------+--------------+--------------+---------------╯

"#]]);

    // Assert 100% coverage (true/false branches properly covered).
    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+---------------+---------------╮
| File              | % Lines       | % Statements  | % Branches    | % Funcs       |
+===================================================================================+
| src/AContract.sol | 100.00% (5/5) | 100.00% (5/5) | 100.00% (2/2) | 100.00% (1/1) |
|-------------------+---------------+---------------+---------------+---------------|
| Total             | 100.00% (5/5) | 100.00% (5/5) | 100.00% (2/2) | 100.00% (1/1) |
╰-------------------+---------------+---------------+---------------+---------------╯

"#]]);
});

// https://github.com/foundry-rs/foundry/issues/10792
forgetest!(branch_with_storage_bytes_reads, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    bytes public optA = bytes("optA");
    bytes public optB = bytes("optB");

    function execute(uint16 value) external view {
        bytes memory element;
        if (value == 4) {
            element = optA;
        } else {
            element = optB;
        }
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a = new AContract();

    function testTrueCoverage() external view {
        a.execute(4);
    }

    function testFalseCoverage() external view {
        a.execute(5);
    }
}
    "#,
    );

    cmd.arg("coverage").args(["--mt", "testTrueCoverage"]).assert_success().stdout_eq(str![[r#"
...
╭-------------------+--------------+--------------+--------------+---------------╮
| File              | % Lines      | % Statements | % Branches   | % Funcs       |
+================================================================================+
| src/AContract.sol | 80.00% (4/5) | 75.00% (3/4) | 50.00% (1/2) | 100.00% (1/1) |
|-------------------+--------------+--------------+--------------+---------------|
| Total             | 80.00% (4/5) | 75.00% (3/4) | 50.00% (1/2) | 100.00% (1/1) |
╰-------------------+--------------+--------------+--------------+---------------╯

"#]]);

    cmd.forge_fuse()
        .arg("coverage")
        .args(["--mt", "testFalseCoverage"])
        .assert_success()
        .stdout_eq(str![[r#"
...
╭-------------------+--------------+--------------+--------------+---------------╮
| File              | % Lines      | % Statements | % Branches   | % Funcs       |
+================================================================================+
| src/AContract.sol | 80.00% (4/5) | 75.00% (3/4) | 50.00% (1/2) | 100.00% (1/1) |
|-------------------+--------------+--------------+--------------+---------------|
| Total             | 80.00% (4/5) | 75.00% (3/4) | 50.00% (1/2) | 100.00% (1/1) |
╰-------------------+--------------+--------------+--------------+---------------╯

"#]]);

    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+---------------+---------------╮
| File              | % Lines       | % Statements  | % Branches    | % Funcs       |
+===================================================================================+
| src/AContract.sol | 100.00% (5/5) | 100.00% (4/4) | 100.00% (2/2) | 100.00% (1/1) |
|-------------------+---------------+---------------+---------------+---------------|
| Total             | 100.00% (5/5) | 100.00% (4/4) | 100.00% (2/2) | 100.00% (1/1) |
╰-------------------+---------------+---------------+---------------+---------------╯

"#]]);
});

forgetest!(branch_with_code_free_else, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    uint256 public value;

    function execute(bool condition) external {
        if (condition) {
            value = 1;
        } else {
            uint256 unused;
        }
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a = new AContract();

    function testCoverage() external {
        a.execute(true);
        a.execute(false);
    }
}
    "#,
    );

    cmd.arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+--------------+--------------+---------------+---------------╮
| File              | % Lines      | % Statements | % Branches    | % Funcs       |
+=================================================================================+
| src/AContract.sol | 75.00% (3/4) | 50.00% (1/2) | 100.00% (2/2) | 100.00% (1/1) |
|-------------------+--------------+--------------+---------------+---------------|
| Total             | 75.00% (3/4) | 50.00% (1/2) | 100.00% (2/2) | 100.00% (1/1) |
╰-------------------+--------------+--------------+---------------+---------------╯

"#]]);
});

forgetest!(identical_bytecodes, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    uint256 public number;
    address public immutable usdc1;
    address public immutable usdc2;
    address public immutable usdc3;
    address public immutable usdc4;
    address public immutable usdc5;
    address public immutable usdc6;

    constructor() {
        address a = 0x176211869cA2b568f2A7D4EE941E073a821EE1ff;
        usdc1 = a;
        usdc2 = a;
        usdc3 = a;
        usdc4 = a;
        usdc5 = a;
        usdc6 = a;
    }

    function setNumber(uint256 newNumber) public {
        number = newNumber;
    }

    function increment() public {
        number++;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract public counter;

    function setUp() public {
        counter = new AContract();
        counter.setNumber(0);
    }

    function test_Increment() public {
        counter.increment();
        assertEq(counter.number(), 1);
    }
}
    "#,
    );

    cmd.arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+-----------------+---------------+------------+---------------╮
| File              | % Lines         | % Statements  | % Branches | % Funcs       |
+==================================================================================+
| src/AContract.sol | 100.00% (12/12) | 100.00% (9/9) | N/A (0/0)  | 100.00% (3/3) |
|-------------------+-----------------+---------------+------------+---------------|
| Total             | 100.00% (12/12) | 100.00% (9/9) | N/A (0/0)  | 100.00% (3/3) |
╰-------------------+-----------------+---------------+------------+---------------╯

"#]]);
});

forgetest!(constructors, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    bool public active;

    constructor() {
        active = true;
    }
}

contract BContract {
    bool public active;

    constructor() {
        active = true;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import "./AContract.sol";

contract AContractTest is DSTest {
    function test_constructors() public {
        AContract a = new AContract();
        BContract b = new BContract();
    }
}
    "#,
    );

    cmd.arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+------------+---------------╮
| File              | % Lines       | % Statements  | % Branches | % Funcs       |
+================================================================================+
| src/AContract.sol | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
|-------------------+---------------+---------------+------------+---------------|
| Total             | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
╰-------------------+---------------+---------------+------------+---------------╯

"#]]);
});

// https://github.com/foundry-rs/foundry/issues/9270,
// https://github.com/foundry-rs/foundry/issues/9444,
// https://github.com/foundry-rs/foundry/issues/9458
// Test coverage for functions with no statements.
forgetest!(empty_functions, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    constructor() {}

    receive() external payable {}

    fallback() external {}

    function increment() public {}
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import "./AContract.sol";

contract AContractTest is DSTest {
    function test_constructors() public {
        AContract a = new AContract();
        a.increment();
        (bool success,) = address(a).call{value: 1}("");
        require(success);
        (success,) = address(a).call(hex"deadbeef");
        require(success);
    }
}
    "#,
    );

    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/AContract.sol
DA:5,1
FN:5,AContract.constructor
FNDA:1,AContract.constructor
DA:7,1
FN:7,AContract.receive
FNDA:1,AContract.receive
DA:9,1
FN:9,AContract.fallback
FNDA:1,AContract.fallback
DA:11,1
FN:11,AContract.increment
FNDA:1,AContract.increment
FNF:4
FNH:4
LF:4
LH:4
BRF:0
BRH:0
end_of_record

"#]],
    );

    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+--------------+------------+---------------╮
| File              | % Lines       | % Statements | % Branches | % Funcs       |
+===============================================================================+
| src/AContract.sol | 100.00% (4/4) | N/A (0/0)    | N/A (0/0)  | 100.00% (4/4) |
|-------------------+---------------+--------------+------------+---------------|
| Total             | 100.00% (4/4) | N/A (0/0)    | N/A (0/0)  | 100.00% (4/4) |
╰-------------------+---------------+--------------+------------+---------------╯

"#]]);
});

// Test empty shared-memory calldata used by nested calls with isolation disabled.
forgetest!(empty_shared_calldata, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    receive() external payable {}
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import "./AContract.sol";

contract AContractTest is DSTest {
    address internal target;

    function setUp() public {
        target = address(new AContract());
    }

    function test_receive() public {
        (bool success,) = target.call{gas: 100_000}("");
        require(success);
    }
}
    "#,
    );

    let expected = str![[r#"
TN:
SF:src/AContract.sol
DA:5,1
FN:5,AContract.receive
FNDA:1,AContract.receive
FNF:1
FNH:1
LF:1
LH:1
BRF:0
BRH:0
end_of_record

"#]];
    assert_lcov(cmd.arg("coverage").arg("--no-isolate"), expected.clone());
    assert_lcov(cmd.forge_fuse().arg("coverage").args(["--no-isolate", "--ir-minimum"]), expected);
});

// Test that inherited empty constructors, receive functions, and fallbacks have distinct anchors.
forgetest!(empty_special_functions_are_distinct, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract Base {
    constructor() {}

    receive() external payable {}

    fallback() external {}

    function increment() public {}
}

contract AContract is Base {}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import "./AContract.sol";

contract AContractTest is DSTest {
    function test_receive() public {
        AContract a = new AContract();
        (bool success,) = address(a).call{value: 1}("");
        require(success);
    }
}
    "#,
    );

    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/AContract.sol
DA:5,1
FN:5,Base.constructor
FNDA:1,Base.constructor
DA:7,1
FN:7,Base.receive
FNDA:1,Base.receive
DA:9,0
FN:9,Base.fallback
FNDA:0,Base.fallback
DA:11,0
FN:11,Base.increment
FNDA:0,Base.increment
FNF:4
FNH:2
LF:4
LH:2
BRF:0
BRH:0
end_of_record

"#]],
    );
});

// Test that a fallback without a receive uses the same anchor for empty and non-empty calldata.
forgetest!(empty_fallback_without_receive, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    fallback() external payable {}
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import "./AContract.sol";

contract AContractTest is DSTest {
    function test_fallback() public {
        AContract a = new AContract();
        (bool success,) = address(a).call{value: 1}("");
        require(success);
        (success,) = address(a).call(hex"01");
        require(success);
        (success,) = address(a).call(hex"deadbeef");
        require(success);
    }
}
    "#,
    );

    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/AContract.sol
DA:5,3
FN:5,AContract.fallback
FNDA:3,AContract.fallback
FNF:1
FNH:1
LF:1
LH:1
BRF:0
BRH:0
end_of_record

"#]],
    );
});

// Test that a nonpayable fallback is not covered by a rejected value-bearing call.
forgetest!(empty_nonpayable_fallback_rejects_value, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    fallback() external {}
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import "./AContract.sol";

contract AContractTest is DSTest {
    function test_rejected_fallback() public {
        AContract a = new AContract();
        (bool success,) = address(a).call{value: 1}(hex"deadbeef");
        require(!success);
    }
}
    "#,
    );

    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/AContract.sol
DA:5,0
FN:5,AContract.fallback
FNDA:0,AContract.fallback
FNF:1
FNH:0
LF:1
LH:0
BRF:0
BRH:0
end_of_record

"#]],
    );
});

// Test coverage for `receive` functions.
forgetest!(receive, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    uint256 public counter = 0;

    constructor() {
        counter = 1;
    }

    receive() external payable {
        counter = msg.value;
    }

    fallback() external {}
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import "./AContract.sol";

contract AContractTest is DSTest {
    AContract a = new AContract();

    function test_constructors() public {
        address(a).call{value: 5}("");
        require(a.counter() == 5);
    }
}
    "#,
    );

    // Assert the constructor and receive are covered while the empty fallback is not.
    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/AContract.sol
DA:7,1
FN:7,AContract.constructor
FNDA:1,AContract.constructor
DA:8,1
DA:11,1
FN:11,AContract.receive
FNDA:1,AContract.receive
DA:12,1
DA:15,0
FN:15,AContract.fallback
FNDA:0,AContract.fallback
FNF:3
FNH:2
LF:5
LH:4
BRF:0
BRH:0
end_of_record

"#]],
    );

    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+--------------+---------------+------------+--------------╮
| File              | % Lines      | % Statements  | % Branches | % Funcs      |
+==============================================================================+
| src/AContract.sol | 80.00% (4/5) | 100.00% (2/2) | N/A (0/0)  | 66.67% (2/3) |
|-------------------+--------------+---------------+------------+--------------|
| Total             | 80.00% (4/5) | 100.00% (2/2) | N/A (0/0)  | 66.67% (2/3) |
╰-------------------+--------------+---------------+------------+--------------╯

"#]]);
});

// Test empty constructor coverage when via-IR source maps contain no matching span.
forgetest!(empty_constructor_ir_minimum, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    constructor() {}
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import "./AContract.sol";

contract AContractTest is DSTest {
    function test_constructor() public {
        new AContract();
    }
}
    "#,
    );

    assert_lcov(
        cmd.arg("coverage").arg("--ir-minimum"),
        str![[r#"
TN:
SF:src/AContract.sol
DA:5,1
FN:5,AContract.constructor
FNDA:1,AContract.constructor
FNF:1
FNH:1
LF:1
LH:1
BRF:0
BRH:0
end_of_record

"#]],
    );
});

// https://github.com/foundry-rs/foundry/issues/9322
// Test coverage with `--ir-minimum` for solidity < 0.8.5.
forgetest!(ir_minimum_early, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
pragma solidity 0.8.4;

contract AContract {
    function isContract(address account) internal view returns (bool) {
        bytes32 codehash;
        bytes32 accountHash = 0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470;
        assembly {
            codehash := extcodehash(account)
        }
        return (codehash != accountHash && codehash != 0x0);
    }
}
    "#,
    );

    // Assert coverage doesn't fail with `Error: Unknown key "inliner"`.
    cmd.arg("coverage").arg("--ir-minimum").assert_success().stdout_eq(str![[r#"
...
╭-------------------+-------------+--------------+------------+-------------╮
| File              | % Lines     | % Statements | % Branches | % Funcs     |
+===========================================================================+
| src/AContract.sol | 0.00% (0/5) | 0.00% (0/4)  | N/A (0/0)  | 0.00% (0/1) |
|-------------------+-------------+--------------+------------+-------------|
| Total             | 0.00% (0/5) | 0.00% (0/4)  | N/A (0/0)  | 0.00% (0/1) |
╰-------------------+-------------+--------------+------------+-------------╯

"#]]);
});

forgetest!(no_artifacts_written, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    int public i;

    function init() public {
        i = 0;
    }

    function foo() public {
        i = 1;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a;

    function setUp() public {
        a = new AContract();
        a.init();
    }

    function testFoo() public {
        a.foo();
    }
}
    "#,
    );

    cmd.forge_fuse().arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-------------------+---------------+---------------+------------+---------------╮
| File              | % Lines       | % Statements  | % Branches | % Funcs       |
+================================================================================+
| src/AContract.sol | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
|-------------------+---------------+---------------+------------+---------------|
| Total             | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
╰-------------------+---------------+---------------+------------+---------------╯
...
"#]]);

    // no artifacts are to be written
    let files = files_with_ext(prj.artifacts(), "json").collect::<Vec<_>>();

    assert!(files.is_empty());
});

forgetest!(attribution_report, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    int public i;

    function init() public {
        i = 0;
    }

    function foo() public {
        i = 1;
    }

    function bar() public {
        i = 2;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a;

    function setUp() public {
        a = new AContract();
        a.init();
    }

    function testFoo() public {
        a.foo();
    }

    function testBar() public {
        a.bar();
    }
}
    "#,
    );

    cmd.arg("coverage").args(["--report=attribution"]).assert_success();

    let attribution = prj.root().join("coverage-attribution.json");
    assert!(attribution.exists(), "coverage attribution report was not created");

    let custom_attribution = prj.root().join("custom-coverage-attribution.json");
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--report=attribution", "--report-file", "custom-coverage-attribution.json"])
        .assert_success();
    assert!(custom_attribution.exists(), "custom coverage attribution report was not created");

    let json: Value = serde_json::from_str(&std::fs::read_to_string(attribution).unwrap()).unwrap();
    let tests = json["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 2);

    for test in tests {
        assert_eq!(test["status"], "success");
        assert_eq!(test["kind"], "unit");

        let covered = test["covered"].as_array().unwrap();
        assert!(
            covered.iter().any(|item| item["source"] == "src/AContract.sol"
                && item["kind"] == "function"
                && item["function"] == "init"),
            "setUp coverage should be attributed to each selected test"
        );
    }

    let test_foo = tests.iter().find(|test| test["test"] == "testFoo()").unwrap();
    assert!(test_foo["covered"].as_array().unwrap().iter().any(|item| {
        item["source"] == "src/AContract.sol"
            && item["kind"] == "function"
            && item["function"] == "foo"
    }));

    let test_bar = tests.iter().find(|test| test["test"] == "testBar()").unwrap();
    assert!(test_bar["covered"].as_array().unwrap().iter().any(|item| {
        item["source"] == "src/AContract.sol"
            && item["kind"] == "function"
            && item["function"] == "bar"
    }));
});

forgetest!(attribution_report_keeps_items_aligned_after_filtering_sources, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "sub/WContract.sol",
        r#"
contract WContract {
    function hit() public pure returns (uint256) {
        return 2;
    }
}
    "#,
    );
    prj.add_source(
        "XContract.sol",
        r#"
contract XContract {
    function hit() public pure returns (uint256) {
        return 4;
    }
}
    "#,
    );
    prj.add_source(
        "YContract.sol",
        r#"
contract YContract {
    function hit() public pure returns (uint256) {
        return 6;
    }
}
    "#,
    );
    prj.add_source(
        "ZContract.sol",
        r#"
contract ZContract {
    function hit() public pure returns (uint256) {
        return 8;
    }
}
    "#,
    );

    prj.add_source(
        "AttributionFilterTest.sol",
        r#"
import "./test.sol";
import {WContract} from "./sub/WContract.sol";
import {XContract} from "./XContract.sol";
import {YContract} from "./YContract.sol";
import {ZContract} from "./ZContract.sol";

contract AttributionFilterTest is DSTest {
    function testW() public {
        assertEq(new WContract().hit(), 2);
    }

    function testX() public {
        assertEq(new XContract().hit(), 4);
    }

    function testY() public {
        assertEq(new YContract().hit(), 6);
    }

    function testZ() public {
        assertEq(new ZContract().hit(), 8);
    }
}
    "#,
    );

    cmd.arg("coverage")
        .args(["--report=attribution", "--no-match-coverage=YContract"])
        .assert_success();

    let attribution = prj.root().join("coverage-attribution.json");
    let json: Value = serde_json::from_str(&std::fs::read_to_string(attribution).unwrap()).unwrap();
    let tests = json["tests"].as_array().unwrap();

    assert!(
        tests.iter().flat_map(|test| test["covered"].as_array().unwrap()).all(|item| {
            item["source"].as_str().is_some_and(|source| !source.contains("YContract.sol"))
        }),
        "filtered source should not be present in attribution output"
    );

    for (test_name, contract_name) in
        [("testW()", "WContract"), ("testX()", "XContract"), ("testZ()", "ZContract")]
    {
        let test = tests.iter().find(|test| test["test"] == test_name).unwrap();
        let covered = test["covered"].as_array().unwrap();
        assert!(
            covered.iter().any(|item| {
                item["source"]
                    .as_str()
                    .is_some_and(|source| source.ends_with(&format!("{contract_name}.sol")))
                    && item["contract"] == contract_name
                    && item["kind"] == "function"
                    && item["function"] == "hit"
                    && item["hits"].as_u64().is_some_and(|hits| hits > 0)
            }),
            "{test_name} should retain coverage for {contract_name}.hit"
        );
    }
});

forgetest!(attribution_report_with_lcov_uses_default_paths, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "AContract.sol",
        r#"
contract AContract {
    int public i;

    function foo() public {
        i = 1;
    }
}
    "#,
    );

    prj.add_source(
        "AContractTest.sol",
        r#"
import "./test.sol";
import {AContract} from "./AContract.sol";

contract AContractTest is DSTest {
    AContract a;

    function setUp() public {
        a = new AContract();
    }

    function testFoo() public {
        a.foo();
    }
}
    "#,
    );

    let stderr = cmd
        .arg("coverage")
        .args(["--report=attribution", "--report=lcov", "--report-file", "combined.out"])
        .assert_success()
        .get_output()
        .stderr_lossy();

    assert!(
        stderr.contains("`--report-file` is ignored when multiple file reports are requested"),
        "expected warning about ignored --report-file, got:\n{stderr}"
    );

    assert!(prj.root().join("coverage-attribution.json").exists());
    assert!(prj.root().join("lcov.info").exists());
    assert!(!prj.root().join("combined.out").exists());
});

// <https://github.com/foundry-rs/foundry/issues/10172>
forgetest!(constructor_with_args, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "ArrayCondition.sol",
        r#"
contract ArrayCondition {
    uint8 public constant MAX_SIZE = 32;
    error TooLarge();
    error EmptyArray();
    // Storage variable to ensure the constructor does something
    uint256 private _arrayLength;

    constructor(uint256[] memory values) {
        // Check for empty array
        if (values.length == 0) {
            revert EmptyArray();
        }

        if (values.length > MAX_SIZE) {
            revert TooLarge();
        }

        // Store the array length
        _arrayLength = values.length;
    }

    function getArrayLength() external view returns (uint256) {
        return _arrayLength;
    }
}
    "#,
    );

    prj.add_source(
        "ArrayConditionTest.sol",
        r#"
import "./test.sol";
import {ArrayCondition} from "./ArrayCondition.sol";

interface Vm {
    function expectRevert(bytes4 revertData) external;
}

contract ArrayConditionTest is DSTest {
    Vm constant vm = Vm(HEVM_ADDRESS);

    function testValidSize() public {
        uint256[] memory values = new uint256[](10);
        ArrayCondition condition = new ArrayCondition(values);
        assertEq(condition.getArrayLength(), 10);
    }

    // Test with maximum array size (should NOT revert)
    function testMaxSize() public {
        uint256[] memory values = new uint256[](32);
        ArrayCondition condition = new ArrayCondition(values);
        assertEq(condition.getArrayLength(), 32);
    }

    // Test with too large array size (should revert)
    function testTooLarge() public {
        uint256[] memory values = new uint256[](33);
        vm.expectRevert(ArrayCondition.TooLarge.selector);
        new ArrayCondition(values);
    }

    // Test with empty array (should revert)
    function testEmptyArray() public {
        uint256[] memory values = new uint256[](0);
        vm.expectRevert(ArrayCondition.EmptyArray.selector);
        new ArrayCondition(values);
    }
}
    "#,
    );

    cmd.arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭------------------------+---------------+---------------+---------------+---------------╮
| File                   | % Lines       | % Statements  | % Branches    | % Funcs       |
+========================================================================================+
| src/ArrayCondition.sol | 100.00% (8/8) | 100.00% (6/6) | 100.00% (2/2) | 100.00% (2/2) |
|------------------------+---------------+---------------+---------------+---------------|
| Total                  | 100.00% (8/8) | 100.00% (6/6) | 100.00% (2/2) | 100.00% (2/2) |
╰------------------------+---------------+---------------+---------------+---------------╯
...
"#]]);
});

// https://github.com/foundry-rs/foundry/issues/11432
// Test coverage for linked libraries.
forgetest!(linked_library, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Counter.sol",
        r#"
library LibCounter {
    function increment(uint256 number) external returns (uint256) {
        return number + 1;
    }
}

contract Counter {
    uint256 public number;

    function increment() public {
        number = LibCounter.increment(number);
    }
}
    "#,
    );

    prj.add_source(
        "CounterTest.sol",
        r#"
import "./test.sol";
import {Counter} from "./Counter.sol";

contract CounterTest is DSTest {
    function testIncrement() public {
        Counter counter = new Counter();
        counter.increment();
    }
}
    "#,
    );

    // Assert 100% coverage for linked libraries.
    cmd.arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-----------------+---------------+---------------+------------+---------------╮
| File            | % Lines       | % Statements  | % Branches | % Funcs       |
+==============================================================================+
| src/Counter.sol | 100.00% (4/4) | 100.00% (3/3) | N/A (0/0)  | 100.00% (2/2) |
|-----------------+---------------+---------------+------------+---------------|
| Total           | 100.00% (4/4) | 100.00% (3/3) | N/A (0/0)  | 100.00% (2/2) |
╰-----------------+---------------+---------------+------------+---------------╯
...
"#]]);
});

// <https://github.com/foundry-rs/foundry/issues/10422>
// Test that line hits are properly recorded in lcov report.
forgetest!(do_while_lcov, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Counter.sol",
        r#"
contract Counter {
    uint256 public number = 21;

    function increment() public {
        uint256 i = 0;
        do {
            number++;
            if (number > 20) {
                number -= 2;
            }
        } while (++i < 10);
    }
}
    "#,
    );

    prj.add_source(
        "Counter.t.sol",
        r#"
import "./test.sol";
import "./Counter.sol";

contract CounterTest is DSTest {
    function test_do_while() public {
        Counter counter = new Counter();
        counter.increment();
    }
}
    "#,
    );

    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/Counter.sol
DA:7,1
FN:7,Counter.increment
FNDA:1,Counter.increment
DA:8,1
DA:10,10
DA:11,10
BRDA:11,0,0,6
DA:12,6
DA:14,10
FNF:1
FNH:1
LF:6
LH:6
BRF:1
BRH:1
end_of_record

"#]],
    );
});

// <https://github.com/foundry-rs/foundry/issues/11183>
// Test that overridden functions are disambiguated in the LCOV report.
forgetest!(disambiguate_functions, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Counter.sol",
        r#"
contract Counter {
    uint256 public number;

    function increment() public {
        number++;
    }
    function increment(uint256 amount) public {
        number += amount;
    }
}
    "#,
    );

    prj.add_source(
        "Counter.t.sol",
        r#"
import "./test.sol";
import "./Counter.sol";

contract CounterTest is DSTest {
    function test_overridden() public {
        Counter counter = new Counter();
        counter.increment();
        counter.increment(1);
        counter.increment(2);
        counter.increment(3);
        assertEq(counter.number(), 7);
    }
}
    "#,
    );

    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/Counter.sol
DA:7,1
FN:7,Counter.increment.0
FNDA:1,Counter.increment.0
DA:8,1
DA:10,3
FN:10,Counter.increment.1
FNDA:3,Counter.increment.1
DA:11,3
FNF:2
FNH:2
LF:4
LH:4
BRF:0
BRH:0
end_of_record

"#]],
    );
});

// Test that functions of abstract contracts and interfaces should not count in coverage report.
forgetest!(abstract_contract_and_interface, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Counter.sol",
        r#"
interface ContractIf {
    function setNumber(uint256 newNumber) external;
}

abstract contract AbstractCounter {
    function _setNumber(uint256 newNumber) internal virtual;

    function _incrementNumber(uint256 newNumber) internal virtual returns (uint256 inc) {
        inc = newNumber + 1;
    }
}

contract Counter is AbstractCounter, ContractIf {
    uint256 public number;

    function setNumber(uint256 newNumber) public {
        _setNumber(newNumber);
    }

    function _setNumber(uint256 newNumber) internal override {
        number = _incrementNumber(newNumber);
    }

    function _incrementNumber(uint256 newNumber) internal override returns (uint256 inc) {
        inc = super._incrementNumber(newNumber);
    }
}
    "#,
    );
    prj.add_source(
        "CounterTest.sol",
        r#"
import "./test.sol";
import {Counter} from "./Counter.sol";

contract CounterTest is DSTest {
    function testCounter() public {
        Counter counter = new Counter();
        counter.setNumber(0);
    }
}
    "#,
    );

    // Test there are 4 functions reported:
    // - `setNumber`, `_setNumber` and `_incrementNumber` from `Counter` contract
    // - `_incrementNumber` from `AbstractCounter` (virtual with implementation). `_setNumber` is
    // excluded as it is not implemented.
    cmd.arg("coverage").assert_success().stdout_eq(str![[r#"
...
╭-----------------+---------------+---------------+------------+---------------╮
| File            | % Lines       | % Statements  | % Branches | % Funcs       |
+==============================================================================+
| src/Counter.sol | 100.00% (8/8) | 100.00% (4/4) | N/A (0/0)  | 100.00% (4/4) |
|-----------------+---------------+---------------+------------+---------------|
| Total           | 100.00% (8/8) | 100.00% (4/4) | N/A (0/0)  | 100.00% (4/4) |
╰-----------------+---------------+---------------+------------+---------------╯
...
"#]]);
});

// <https://github.com/foundry-rs/foundry/issues/11548>
// Test BRDA hit values follow LCOV spec: "-" when line never executed, "0" when line hit but
// branch not taken. This ensures `genhtml` consistency.
forgetest!(brda_lcov_consistency, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Counter.sol",
        r#"
contract Counter {
    uint256 public number;

    function setPositive(uint256 newNumber) public {
        if (newNumber > 0) {
            number = newNumber;
        } else {
            number = 1;
        }
    }

    function neverCalled(uint256 x) public {
        if (x > 100) {
            number = x;
        } else {
            number = 100;
        }
    }
}
    "#,
    );

    prj.add_source(
        "Counter.t.sol",
        r#"
import "./test.sol";
import "./Counter.sol";

contract CounterTest is DSTest {
    function test_only_positive_branch() public {
        Counter counter = new Counter();
        counter.setPositive(42);
        counter.setPositive(100);
    }
}
    "#,
    );

    // Verify BRDA values:
    // - BRDA:8,0,0,2 - if branch taken 2 times
    // - BRDA:8,0,1,0 - else branch NOT taken but line was hit (outputs "0", not "-")
    // - BRDA:16,1,0,- - if branch NOT taken AND line never executed (outputs "-")
    // - BRDA:16,1,1,- - else branch NOT taken AND line never executed (outputs "-")
    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/Counter.sol
DA:7,2
FN:7,Counter.setPositive
FNDA:2,Counter.setPositive
DA:8,2
BRDA:8,0,0,2
BRDA:8,0,1,0
DA:9,2
DA:11,0
DA:15,0
FN:15,Counter.neverCalled
FNDA:0,Counter.neverCalled
DA:16,0
BRDA:16,1,0,-
BRDA:16,1,1,-
DA:17,0
DA:19,0
FNF:2
FNH:1
LF:8
LH:3
BRF:4
BRH:1
end_of_record

"#]],
    );
});

// Test that coverage files are written even when tests fail.
forgetest!(coverage_with_failing_tests, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Counter.sol",
        r#"
contract Counter {
    uint256 public number;

    function setNumber(uint256 newNumber) public {
        number = newNumber;
    }

    function increment() public {
        number++;
    }
}
    "#,
    );

    prj.add_source(
        "CounterTest.sol",
        r#"
import "./test.sol";
import {Counter} from "./Counter.sol";

contract CounterTest is DSTest {
    Counter public counter;

    function setUp() public {
        counter = new Counter();
        counter.setNumber(0);
    }

    function test_Increment() public {
        counter.increment();
        assertEq(counter.number(), 1);
    }

    function test_FailingTest() public {
        counter.increment();
        // This assertion will fail
        assertEq(counter.number(), 999);
    }
}
    "#,
    );

    // Run coverage - this should exit with error code 1 due to failing test,
    // but the lcov file should still be written.
    cmd.arg("coverage").args(["--report=lcov"]).assert_failure();

    // Verify that the lcov.info file was created despite test failure
    let lcov = prj.root().join("lcov.info");
    assert!(lcov.exists(), "lcov.info should be created even when tests fail");

    // Verify the coverage data is valid and includes the counter contract
    let lcov_content = std::fs::read_to_string(&lcov).unwrap();
    assert!(lcov_content.contains("SF:src/Counter.sol"), "Coverage should include Counter.sol");
    assert!(lcov_content.contains("FN:"), "Coverage should include function data");
    assert!(lcov_content.contains("DA:"), "Coverage should include line hit data");
});

forgetest_init!(coverage_rejects_mutation_mode_before_compile, |prj, cmd| {
    prj.add_source(
        "Broken.sol",
        r#"
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

contract Broken {
    function broken() public pure returns (uint256) {
        return;
    }
}
"#,
    );

    let output = cmd.args(["coverage", "--mutate", "src/Broken.sol"]).assert_failure();
    let stderr = output.get_output().stderr_lossy();

    assert!(
        stderr.contains("`--mutate` cannot be combined with: coverage"),
        "unexpected stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("Compiler run failed"),
        "mutation/coverage conflict should be reported before compile errors:\n{stderr}"
    );
});

forgetest_init!(coverage_rejects_test_only_modes_before_compile, |prj, cmd| {
    prj.add_source(
        "Broken.sol",
        r#"
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

contract Broken {
    function broken() public pure returns (uint256) {
        return;
    }
}
"#,
    );

    for args in [
        &["--json"][..],
        &["--junit"],
        &["--list"],
        &["--debug"],
        &["--flamegraph"],
        &["--flamechart"],
        &["--evm-profile"],
        &["--showmap-out", "showmap"],
        &["--brutalize"],
        &["--replay-symbolic-artifact", "counterexample.json"],
        &["--watch", "--json"],
    ] {
        let output = cmd.forge_fuse().arg("coverage").args(args).assert_failure();
        let stderr = output.get_output().stderr_lossy();

        assert!(
            stderr.contains("`forge coverage` cannot be combined with:"),
            "unexpected stderr for {args:?}:\n{stderr}"
        );
        assert!(
            !stderr.contains("Compiler run failed"),
            "coverage conflict should be reported before compile errors for {args:?}:\n{stderr}"
        );
    }
});

forgetest_init!(coverage_match_path_compiles_selected_tests, |prj, cmd| {
    prj.add_test(
        "Shared.t.sol",
        r#"
abstract contract SharedTest {
    function sharedValue() internal pure returns (uint256) {
        return 1;
    }
}
"#,
    );
    prj.add_test(
        "Selected.t.sol",
        r#"
import {SharedTest} from "./Shared.t.sol";

contract SelectedTest is SharedTest {
    function testSelected() public pure {
        require(sharedValue() == 1);
    }
}
"#,
    );
    prj.add_test(
        "Broken.t.sol",
        r#"
contract BrokenTest {
    function broken() public pure returns (uint256) {
        return;
    }
}
"#,
    );
    prj.add_test(
        "Unselected.t.sol",
        r#"
contract UnselectedTest {
    function testUnselected() public {}
}
"#,
    );
    prj.add_script(
        "Report.s.sol",
        r#"
contract ReportScript {
    function run() public {}
}
"#,
    );

    let output = cmd
        .args(["coverage", "--match-path", "test/Selected.t.sol"])
        .assert_success()
        .get_output()
        .stdout_lossy();
    assert!(output.contains("testSelected()"), "selected test did not run:\n{output}");
    assert!(
        output.contains("script/Report.s.sol"),
        "scripts should remain in path-filtered coverage reports:\n{output}"
    );
    assert!(
        !output.contains("test/Unselected.t.sol"),
        "unselected tests should be omitted from coverage reports:\n{output}"
    );
});

// <https://github.com/foundry-rs/foundry/issues/16084>
forgetest!(coverage_separates_compiler_builds, |prj, cmd| {
    prj.update_config(|config| {
        config.additional_compiler_profiles = vec![SettingsOverrides {
            name: "via-ir".to_owned(),
            via_ir: Some(true),
            evm_version: None,
            optimizer: None,
            optimizer_runs: None,
            bytecode_hash: None,
        }];
        config.compilation_restrictions = vec![CompilationRestrictions {
            paths: "src/A.sol".parse().unwrap(),
            version: None,
            via_ir: Some(true),
            bytecode_hash: None,
            min_optimizer_runs: None,
            optimizer_runs: None,
            max_optimizer_runs: None,
            min_evm_version: None,
            evm_version: None,
            max_evm_version: None,
        }];
    });

    prj.add_source(
        "A.sol",
        "contract A { function value() external pure returns (uint256) { return 1; } }",
    );
    prj.add_source(
        "B.sol",
        "contract B { function value() external pure returns (uint256) { return 2; } }",
    );
    prj.add_test(
        "Coverage.t.sol",
        r#"
import {A} from "../src/A.sol";
import {B} from "../src/B.sol";
contract CoverageTest {
    function testCoverage() public {
        require(new A().value() == 1);
        require(new B().value() == 2);
    }
}
"#,
    );

    assert_lcov(
        cmd.arg("coverage"),
        str![[r#"
TN:
SF:src/A.sol
DA:3,1
FN:3,A.value
FNDA:1,A.value
FNF:1
FNH:1
LF:1
LH:1
BRF:0
BRH:0
end_of_record
TN:
SF:src/B.sol
DA:3,1
FN:3,B.value
FNDA:1,B.value
FNF:1
FNH:1
LF:1
LH:1
BRF:0
BRH:0
end_of_record

"#]],
    );
});

// A file-level (free) function declared after a contract must be attributed at file level, not
// leaked into the earlier contract's scope, and its deferred call must carry the same scope.
// Regression for https://github.com/foundry-rs/foundry/issues/16085
forgetest!(coverage_reports_free_functions, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Mixed.sol",
        r#"
contract Counter {
    function bump(uint256 x) public pure returns (uint256) {
        return triple(x);
    }
}

function triple(uint256 x) pure returns (uint256) {
    return x * 3;
}
"#,
    );
    prj.add_source(
        "MixedTest.sol",
        r#"
import "./test.sol";
import {Counter} from "./Mixed.sol";

contract MixedTest is DSTest {
    function testMixed() public {
        Counter c = new Counter();
        require(c.bump(2) == 6);
    }
}
"#,
    );

    cmd.args(["coverage", "--report=lcov", "--report=attribution", "--lcov-version=2"])
        .assert_success();
    let lcov = std::fs::read_to_string(prj.root().join("lcov.info")).unwrap();
    let attribution =
        std::fs::read_to_string(prj.root().join("coverage-attribution.json")).unwrap();
    let attribution: Value = serde_json::from_str(&attribution).unwrap();

    assert!(lcov.contains("SF:src/Mixed.sol"), "coverage must include Mixed.sol:\n{lcov}");
    // The contract method keeps its contract scope.
    assert!(
        lcov.lines().any(|l| l.starts_with("FN") && l.ends_with(",Counter.bump")),
        "contract function must be scoped as `Counter.bump`:\n{lcov}"
    );
    // The free function is attributed at file level: bare `triple`, with no contract prefix
    // (`Counter.triple`) and no malformed empty scope (`.triple`).
    assert!(
        lcov.lines().any(|l| l.starts_with("FN") && l.ends_with(",triple")),
        "free function must be attributed as bare `triple`:\n{lcov}"
    );
    assert!(
        !lcov.contains(",Counter.triple"),
        "free function must not leak the contract scope:\n{lcov}"
    );
    assert!(
        !lcov.contains(",.triple"),
        "free function must not have a malformed empty scope:\n{lcov}"
    );
    // Both functions were executed, so both record a nonzero hit count.
    assert!(
        lcov.lines()
            .any(|l| l.starts_with("FNDA:") && !l.starts_with("FNDA:0,") && l.ends_with(",triple")),
        "free function must record its hits:\n{lcov}"
    );
    assert!(
        lcov.lines().any(|l| l.starts_with("FNDA:")
            && !l.starts_with("FNDA:0,")
            && l.ends_with(",Counter.bump")),
        "contract function must record its hits:\n{lcov}"
    );
    let covered = attribution["tests"][0]["covered"].as_array().unwrap();
    assert_eq!(
        covered
            .iter()
            .filter(|item| {
                item["source"] == "src/Mixed.sol"
                    && item["contract"] == "Counter"
                    && item["kind"] == "statement"
            })
            .count(),
        2,
        "the return statement and its deferred call must retain the contract scope:\n{attribution}"
    );
});

// A file whose only code is a file-level (free) function must still be reported, with the
// function named at file level. Before the fix such a file was dropped from coverage entirely.
// Regression for https://github.com/foundry-rs/foundry/issues/16085
forgetest!(coverage_reports_free_only_functions, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Free.sol",
        r#"
function double(uint256 x) pure returns (uint256) {
    return x * 2;
}
"#,
    );
    prj.add_source(
        "FreeTest.sol",
        r#"
import "./test.sol";
import {double} from "./Free.sol";

contract FreeTest is DSTest {
    function testDouble() public {
        require(double(3) == 6);
    }
}
"#,
    );

    cmd.args(["coverage", "--report=lcov", "--lcov-version=2"]).assert_success();
    let lcov = std::fs::read_to_string(prj.root().join("lcov.info")).unwrap();

    // Inspect the Free.sol record in isolation so FNF/FNH counts are unambiguous.
    let free_block = lcov
        .split("SF:")
        .find(|b| b.starts_with("src/Free.sol"))
        .expect("free-function-only file must have a coverage record");
    // The function is named `double`, not `.double`.
    assert!(
        free_block.lines().any(|l| l.starts_with("FN") && l.ends_with(",double")),
        "free function must be reported as bare `double`:\n{free_block}"
    );
    assert!(!free_block.contains(".double"), "no leading-dot empty scope:\n{free_block}");
    // Exactly one function, found and hit.
    assert!(free_block.contains("FNF:1"), "exactly one function found:\n{free_block}");
    assert!(free_block.contains("FNH:1"), "exactly one function hit:\n{free_block}");
});

// A contract method and a file-level function that share a name live in different scopes, so
// neither should be renamed as if it were an overload of the other.
forgetest!(coverage_disambiguates_functions_by_scope, |prj, cmd| {
    prj.insert_ds_test();
    prj.add_source(
        "Same.sol",
        r#"
contract Lib {
    function value() public pure returns (uint256) {
        return 1;
    }
}

function value() pure returns (uint256) {
    return 2;
}
"#,
    );
    prj.add_source(
        "SameTest.sol",
        r#"
import "./test.sol";
import {Lib, value} from "./Same.sol";

contract SameTest is DSTest {
    function testValue() public {
        Lib lib = new Lib();
        require(lib.value() == 1);
        require(value() == 2);
    }
}
"#,
    );

    cmd.args(["coverage", "--report=lcov", "--lcov-version=2"]).assert_success();
    let lcov = std::fs::read_to_string(prj.root().join("lcov.info")).unwrap();

    // The contract method stays scoped; the free function stays bare.
    assert!(
        lcov.lines().any(|l| l.starts_with("FN") && l.ends_with(",Lib.value")),
        "contract method must be `Lib.value`:\n{lcov}"
    );
    assert!(
        lcov.lines().any(|l| l.starts_with("FN") && l.ends_with(",value")),
        "free function must be bare `value`:\n{lcov}"
    );
    // Neither is a real overload, so neither gets a `.0` / `.1` disambiguation suffix.
    assert!(
        !lcov.contains("value.0") && !lcov.contains("value.1"),
        "same-named functions in different scopes must not be disambiguated as overloads:\n{lcov}"
    );
});

#[test]
fn coverage_help_renders_notes() {
    let help = CoverageArgs::command().render_long_help().to_string();

    assert!(help.contains(concat!(
        "Source attribution:\n",
        "  Coverage follows compiler source maps. Inherited modifier code is reported under"
    )));
    assert!(help.contains("use `--include-libs` to include their coverage."));
    assert!(help.contains(concat!(
        "Compatibility:\n",
        "  `forge coverage` supports test filters and `--watch`, but not test-only output"
    )));
    assert!(!help.contains("\\n"));
}
// =============================================================================
// Instrumented coverage (`forge coverage --instrumented`).
//
// These fixtures isolate one Solidity construct at a time and assert the summary
// line/statement/branch/function counts. Instrumented mode rewrites sources before
// compilation and records hits through a sentinel address, so coverage stays accurate
// regardless of the optimizer or `viaIR`. Branch accounting follows the Istanbul/
// solidity-coverage convention: each branchy construct contributes two outcomes.
// =============================================================================

forgetest!(instrumented_logical_or_branches, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function either(bool a, bool b) external pure returns (bool) {
        return a || b;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testLeftShortCircuits() external view {
        // `a` is true, so `b` is never evaluated: the right path stays uncovered.
        require(t.either(true, false));
    }

    function testBothSides() external view {
        require(t.either(true, false));
        require(t.either(false, true));
    }
}
"#,
    );

    // Only the left operand is exercised => the right branch path is uncovered (50%).
    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testLeftShortCircuits"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testLeftShortCircuits() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+--------------+---------------╮
| File           | % Lines       | % Statements  | % Branches   | % Funcs       |
+===============================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
|----------------+---------------+---------------+--------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
╰----------------+---------------+---------------+--------------+---------------╯

"#]]);

    // Both operands exercised => full branch coverage.
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testBothSides"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testBothSides() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | 100.00% (2/2) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | 100.00% (2/2) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

forgetest!(instrumented_logical_and_branches, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function both(bool a, bool b) external pure returns (bool) {
        return a && b;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testLeftFalseShortCircuits() external view {
        // `a` is false, so `b` is never evaluated: the right path stays uncovered.
        require(!t.both(false, true));
    }

    function testBothSides() external view {
        require(!t.both(false, true));
        require(t.both(true, true));
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testLeftFalseShortCircuits"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testLeftFalseShortCircuits() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+--------------+---------------╮
| File           | % Lines       | % Statements  | % Branches   | % Funcs       |
+===============================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
|----------------+---------------+---------------+--------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
╰----------------+---------------+---------------+--------------+---------------╯

"#]]);

    cmd.forge_fuse()
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testBothSides"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testBothSides() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | 100.00% (2/2) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | 100.00% (2/2) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

forgetest!(instrumented_ternary_branches, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function pick(bool flag) external pure returns (uint256) {
        return flag ? 1 : 2;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testTrueOnly() external view {
        require(t.pick(true) == 1);
    }

    function testFalseOnly() external view {
        require(t.pick(false) == 2);
    }

    function testBothSides() external view {
        require(t.pick(true) == 1);
        require(t.pick(false) == 2);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testTrueOnly"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testTrueOnly() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+--------------+---------------╮
| File           | % Lines       | % Statements  | % Branches   | % Funcs       |
+===============================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
|----------------+---------------+---------------+--------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
╰----------------+---------------+---------------+--------------+---------------╯

"#]]);

    cmd.forge_fuse()
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testFalseOnly"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testFalseOnly() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+--------------+---------------╮
| File           | % Lines       | % Statements  | % Branches   | % Funcs       |
+===============================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
|----------------+---------------+---------------+--------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | 50.00% (1/2) | 100.00% (1/1) |
╰----------------+---------------+---------------+--------------+---------------╯

"#]]);

    cmd.forge_fuse()
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testBothSides"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testBothSides() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | 100.00% (2/2) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | 100.00% (2/2) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

forgetest!(instrumented_if_else_if_chain, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function classify(uint256 x) external pure returns (uint256) {
        if (x == 0) {
            return 1;
        } else if (x == 1) {
            return 2;
        } else {
            return 3;
        }
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testAllPaths() external view {
        require(t.classify(0) == 1);
        require(t.classify(1) == 2);
        require(t.classify(2) == 3);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testAllPaths"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testAllPaths() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (8/8) | 100.00% (3/3) | 100.00% (4/4) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (8/8) | 100.00% (3/3) | 100.00% (4/4) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

forgetest!(instrumented_for_loop_variants, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function standard(uint256 n) external pure returns (uint256 sum) {
        for (uint256 i = 0; i < n; i++) {
            sum += i;
        }
    }

    function noInit(uint256 n) external pure returns (uint256 sum) {
        uint256 i = 0;
        for (; i < n; i++) {
            sum += i;
        }
    }

    function singleStatementBody(uint256 n) external pure returns (uint256 sum) {
        for (uint256 i = 0; i < n; i++) sum += i;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testLoops() external view {
        require(t.standard(3) == 3);
        require(t.noInit(3) == 3);
        require(t.singleStatementBody(3) == 3);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testLoops"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testLoops() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+------------+---------------╮
| File           | % Lines       | % Statements  | % Branches | % Funcs       |
+=============================================================================+
| src/Target.sol | 100.00% (9/9) | 100.00% (6/6) | N/A (0/0)  | 100.00% (3/3) |
|----------------+---------------+---------------+------------+---------------|
| Total          | 100.00% (9/9) | 100.00% (6/6) | N/A (0/0)  | 100.00% (3/3) |
╰----------------+---------------+---------------+------------+---------------╯

"#]]);
});

forgetest!(instrumented_while_and_do_while, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function whileLoop(uint256 n) external pure returns (uint256 count) {
        while (count < n) {
            count++;
        }
    }

    function doWhileLoop(uint256 n) external pure returns (uint256 count) {
        do {
            count++;
        } while (count < n);
    }

    function whileSingleStatement(uint256 n) external pure returns (uint256 count) {
        while (count < n) count++;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testLoops() external view {
        require(t.whileLoop(2) == 2);
        require(t.doWhileLoop(2) == 2);
        require(t.whileSingleStatement(2) == 2);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testLoops"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testLoops() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+------------+---------------╮
| File           | % Lines       | % Statements  | % Branches | % Funcs       |
+=============================================================================+
| src/Target.sol | 100.00% (8/8) | 100.00% (5/5) | N/A (0/0)  | 100.00% (3/3) |
|----------------+---------------+---------------+------------+---------------|
| Total          | 100.00% (8/8) | 100.00% (5/5) | N/A (0/0)  | 100.00% (3/3) |
╰----------------+---------------+---------------+------------+---------------╯

"#]]);
});

forgetest!(instrumented_try_catch, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Callee {
    function maybeRevert(bool flag) external pure returns (uint256) {
        require(flag, "boom");
        return 1;
    }
}

contract Target {
    Callee callee = new Callee();

    function run(bool flag) external returns (uint256) {
        try callee.maybeRevert(flag) returns (uint256 value) {
            return value;
        } catch Error(string memory) {
            return 2;
        }
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testBothClauses() external {
        require(t.run(true) == 1);
        require(t.run(false) == 2);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testBothClauses"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful with warnings:
Warning (2018): Function state mutability can be restricted to view
  [FILE]:14:5:
   |
14 |     function run(bool flag) external returns (uint256) {/// @solidity memory-safe-assembly
   |     ^ (Relevant source part starts here and spans across multiple lines).

Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testBothClauses() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (8/8) | 100.00% (4/4) | 100.00% (4/4) | 100.00% (2/2) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (8/8) | 100.00% (4/4) | 100.00% (4/4) | 100.00% (2/2) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

forgetest!(instrumented_constructor_and_assembly, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    uint256 public value;

    constructor(uint256 initial) {
        value = initial;
    }

    function addOne(uint256 x) external pure returns (uint256 result) {
        assembly {
            result := add(x, 1)
        }
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    function testConstructorAndAssembly() external {
        Target t = new Target(5);
        require(t.value() == 5);
        require(t.addOne(1) == 2);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testConstructorAndAssembly"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testConstructorAndAssembly() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+------------+---------------╮
| File           | % Lines       | % Statements  | % Branches | % Funcs       |
+=============================================================================+
| src/Target.sol | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
|----------------+---------------+---------------+------------+---------------|
| Total          | 100.00% (4/4) | 100.00% (2/2) | N/A (0/0)  | 100.00% (2/2) |
╰----------------+---------------+---------------+------------+---------------╯

"#]]);
});

// `fallback` is instrumented, but `receive` is intentionally skipped: a probe issues a
// `staticcall`, which would exhaust the 2300-gas stipend that `transfer`/`send` give a
// `receive` function. Only `fallback` is therefore counted below (1 function, 1 statement).
forgetest!(instrumented_fallback_instrumented_receive_skipped, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    uint256 public calls;

    fallback() external payable {
        calls++;
    }

    receive() external payable {
        calls++;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    function testFallbackAndReceive() external {
        Target t = new Target();
        (bool ok,) = address(t).call{value: 0}(hex"12345678");
        require(ok);
        (bool ok2,) = address(t).call{value: 1}("");
        require(ok2);
        require(t.calls() == 2);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testFallbackAndReceive"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testFallbackAndReceive() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+------------+---------------╮
| File           | % Lines       | % Statements  | % Branches | % Funcs       |
+=============================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | N/A (0/0)  | 100.00% (1/1) |
|----------------+---------------+---------------+------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | N/A (0/0)  | 100.00% (1/1) |
╰----------------+---------------+---------------+------------+---------------╯

"#]]);
});

// A modifier body is instrumented like any function body, so a `require` guard inside it is
// counted as a branch. Exercising only the passing path leaves the guard's false branch
// uncovered (50%); exercising both reaches full coverage.
forgetest!(instrumented_modifier_guard_branches, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    modifier onlyPositive(uint256 x) {
        require(x > 0, "nonpositive");
        _;
    }

    function f(uint256 x) external onlyPositive(x) returns (uint256) {
        return x;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

interface Vm {
    function expectRevert(bytes calldata revertData) external;
}

contract TargetTest {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    Target t = new Target();

    function testPassingOnly() external {
        require(t.f(1) == 1);
    }

    function testBothGuardPaths() external {
        require(t.f(1) == 1);
        vm.expectRevert(bytes("nonpositive"));
        t.f(0);
    }
}
"#,
    );

    // Passing path only: the guard's false branch is uncovered.
    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testPassingOnly"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful with warnings:
Warning (2018): Function state mutability can be restricted to view
  [FILE]:10:5:
   |
10 |     function f(uint256 x) external onlyPositive(x) returns (uint256) {/// @solidity memory-safe-assembly
   |     ^ (Relevant source part starts here and spans across multiple lines).

Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testPassingOnly() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+--------------+---------------╮
| File           | % Lines       | % Statements  | % Branches   | % Funcs       |
+===============================================================================+
| src/Target.sol | 100.00% (4/4) | 100.00% (2/2) | 50.00% (1/2) | 100.00% (2/2) |
|----------------+---------------+---------------+--------------+---------------|
| Total          | 100.00% (4/4) | 100.00% (2/2) | 50.00% (1/2) | 100.00% (2/2) |
╰----------------+---------------+---------------+--------------+---------------╯

"#]]);

    // Both guard paths exercised.
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testBothGuardPaths"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful with warnings:
Warning (2018): Function state mutability can be restricted to view
  [FILE]:10:5:
   |
10 |     function f(uint256 x) external onlyPositive(x) returns (uint256) {/// @solidity memory-safe-assembly
   |     ^ (Relevant source part starts here and spans across multiple lines).

Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testBothGuardPaths() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (4/4) | 100.00% (2/2) | 100.00% (2/2) | 100.00% (2/2) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (4/4) | 100.00% (2/2) | 100.00% (2/2) | 100.00% (2/2) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

// Instrumented coverage is independent of the optimizer / IR pipeline: it instruments source
// before compilation rather than reading source maps. This fixture compiles the SAME contract
// three ways (default, `--ir-minimum`, and a profile with the optimizer + viaIR enabled) and
// asserts identical, fully-accurate coverage each time -- the property that motivates the mode.
forgetest!(instrumented_independent_of_optimizer, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function classify(uint256 x) external pure returns (uint256) {
        if (x == 0) {
            return 1;
        }
        return x > 10 ? 2 : 3;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testAllPaths() external view {
        require(t.classify(0) == 1);
        require(t.classify(11) == 2);
        require(t.classify(5) == 3);
    }
}
"#,
    );

    // Default profile (optimizer + viaIR disabled by coverage).
    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testAllPaths"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testAllPaths() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (4/4) | 100.00% (2/2) | 100.00% (4/4) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (4/4) | 100.00% (2/2) | 100.00% (4/4) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);

    // `--ir-minimum`: viaIR with minimal optimization.
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--instrumented", "--ir-minimum", "--exclude-tests", "--mt", "testAllPaths"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testAllPaths() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (4/4) | 100.00% (2/2) | 100.00% (4/4) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (4/4) | 100.00% (2/2) | 100.00% (4/4) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);

    // Full optimizer + viaIR via config: coverage must still be accurate.
    prj.update_config(|config| {
        config.optimizer = Some(true);
        config.optimizer_runs = Some(200);
        config.via_ir = true;
    });
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testAllPaths"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testAllPaths() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (4/4) | 100.00% (2/2) | 100.00% (4/4) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (4/4) | 100.00% (2/2) | 100.00% (4/4) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

// `catch Panic(uint256)` and a trailing `catch (bytes)` clause each become branch outcomes
// alongside the success clause.
forgetest!(instrumented_try_catch_panic_and_raw, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Callee {
    function compute(uint256 mode) external pure returns (uint256) {
        if (mode == 1) {
            assert(false);
        }
        if (mode == 2) {
            revert("custom");
        }
        return 7;
    }
}

contract Target {
    Callee callee = new Callee();

    function run(uint256 mode) external returns (uint256) {
        try callee.compute(mode) returns (uint256 value) {
            return value;
        } catch Panic(uint256) {
            return 1;
        } catch (bytes memory) {
            return 2;
        }
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testAllClauses() external {
        require(t.run(0) == 7);
        require(t.run(1) == 1);
        require(t.run(2) == 2);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testAllClauses"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful with warnings:
Warning (2018): Function state mutability can be restricted to view
  [FILE]:19:5:
   |
19 |     function run(uint256 mode) external returns (uint256) {/// @solidity memory-safe-assembly
   |     ^ (Relevant source part starts here and spans across multiple lines).

Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testAllClauses() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+-----------------+---------------+---------------+---------------╮
| File           | % Lines         | % Statements  | % Branches    | % Funcs       |
+==================================================================================+
| src/Target.sol | 100.00% (13/13) | 100.00% (6/6) | 100.00% (7/7) | 100.00% (2/2) |
|----------------+-----------------+---------------+---------------+---------------|
| Total          | 100.00% (13/13) | 100.00% (6/6) | 100.00% (7/7) | 100.00% (2/2) |
╰----------------+-----------------+---------------+---------------+---------------╯

"#]]);
});

// Instrumented coverage feeds the same reporters as the default mode. This fixture checks the
// LCOV tracefile (line `DA`, function `FN`/`FNDA`, and branch `BRDA`/`BRF`/`BRH` records) so a
// regression in report wiring -- not just the summary table -- is caught.
forgetest!(instrumented_lcov_report, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function pick(bool flag) external pure returns (uint256) {
        if (flag) {
            return 1;
        }
        return 2;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testBothPaths() external view {
        require(t.pick(true) == 1);
        require(t.pick(false) == 2);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args([
            "--instrumented",
            "--exclude-tests",
            "--mt",
            "testBothPaths",
            "--report=lcov",
            "--report-file",
        ])
        .assert_file(str![[r#"
TN:
SF:src/Target.sol
DA:5,2
FN:5,Target.pick
FNDA:2,Target.pick
DA:6,1
BRDA:6,0,0,1
BRDA:6,0,1,1
DA:7,1
DA:9,1
FNF:1
FNH:1
LF:4
LH:4
BRF:2
BRH:2
end_of_record

"#]]);
});

forgetest!(instrumented_json_report, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function pick(bool flag) external pure returns (uint256) {
        if (flag) {
            return 1;
        }
        return 2;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testBothPaths() external view {
        require(t.pick(true) == 1);
        require(t.pick(false) == 2);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--report=json"])
        .assert_success();

    let report_path = prj.root().join("coverage-final.json");
    let report: Value = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    let file = report.get("src/Target.sol").expect("missing target source");

    assert_eq!(file["path"], "src/Target.sol");
    assert_eq!(file["statementMap"].as_object().unwrap().len(), 2);
    assert_eq!(file["fnMap"].as_object().unwrap().len(), 1);
    assert_eq!(file["branchMap"].as_object().unwrap().len(), 1);
    assert_eq!(file["f"]["0"], 2);
    assert_eq!(file["b"]["0"], serde_json::json!([1, 1]));
});

forgetest!(instrumented_ignore_file_directive, |prj, cmd| {
    prj.add_source(
        "Ignored.sol",
        r#"
// forge coverage ignore file
contract Ignored {
    function neverCalled() external pure returns (uint256) {
        return 1;
    }
}
"#,
    );
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function covered() external pure returns (uint256) {
        return 2;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testCovered() external view {
        require(t.covered() == 2);
    }
}
"#,
    );

    let output = cmd
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests"])
        .assert_success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&output);
    assert!(!stdout.contains("src/Ignored.sol"), "{stdout}");
    assert!(stdout.contains("src/Target.sol"), "{stdout}");
});

forgetest!(instrumented_ignore_next_directive, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function covered() external pure returns (uint256) {
        return 1;
    }

    // istanbul ignore next
    function ignored() external pure returns (uint256) {
        return 2;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testCovered() external view {
        require(t.covered() == 1);
    }
}
"#,
    );

    let output = cmd
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests"])
        .assert_success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&output);
    assert!(stdout.contains("src/Target.sol"), "{stdout}");
    assert!(stdout.contains("100.00% (1/1)"), "{stdout}");
    assert!(!stdout.contains("50.00% (1/2)"), "{stdout}");
});

forgetest!(instrumented_no_match_coverage_config, |prj, cmd| {
    prj.update_config(|config| {
        config.coverage_pattern_inverse =
            Some(regex::Regex::new(r"(^|[/\\])Ignored\.sol$").unwrap().into());
    });
    prj.add_source(
        "Ignored.sol",
        r#"
contract Ignored {
    function neverCalled() external pure returns (uint256) {
        return 1;
    }
}
"#,
    );
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function covered() external pure returns (uint256) {
        return 2;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testCovered() external view {
        require(t.covered() == 2);
    }
}
"#,
    );

    let output = cmd
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests"])
        .assert_success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&output);
    assert!(!stdout.contains("src/Ignored.sol"), "{stdout}");
    assert!(stdout.contains("src/Target.sol"), "{stdout}");
});

forgetest!(instrumented_multi_solc, |prj, cmd| {
    prj.add_source(
        "Legacy.sol",
        r#"
pragma solidity 0.8.4;

contract Legacy {
    function value() external view returns (uint256) {
        return 1;
    }
}
"#,
    );
    prj.add_source(
        "Modern.sol",
        r#"
pragma solidity 0.8.33;

contract Modern {
    function value() external pure returns (uint256) {
        return 2;
    }
}
"#,
    );
    prj.add_test(
        "LegacyTest.sol",
        r#"
pragma solidity 0.8.4;

import {Legacy} from "../src/Legacy.sol";

contract LegacyTest {
    Legacy legacy = new Legacy();

    function testLegacy() external view {
        require(legacy.value() == 1);
    }
}
"#,
    );
    prj.add_test(
        "ModernTest.sol",
        r#"
pragma solidity 0.8.33;

import {Modern} from "../src/Modern.sol";

contract ModernTest {
    Modern modern = new Modern();

    function testModern() external view {
        require(modern.value() == 2);
    }
}
"#,
    );

    let output = cmd
        .arg("coverage")
        .args(["--instrumented", "--exclude-tests"])
        .assert_success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&output);
    assert!(stdout.contains("src/Legacy.sol"), "{stdout}");
    assert!(stdout.contains("src/Modern.sol"), "{stdout}");
    assert!(stdout.contains("100.00% (4/4)"), "{stdout}");
});

// A ternary nested inside another ternary yields two independent branches (four outcomes),
// and the rewrite preserves evaluation so the returned values stay correct.
forgetest!(instrumented_nested_ternary, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function classify(uint256 x) external pure returns (uint256) {
        return x == 0 ? 0 : (x < 10 ? 1 : 2);
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testAllOutcomes() external view {
        require(t.classify(0) == 0);
        require(t.classify(5) == 1);
        require(t.classify(50) == 2);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testAllOutcomes"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testAllOutcomes() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | 100.00% (4/4) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | 100.00% (4/4) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

// Multiple modifiers on one function: each modifier body is instrumented independently, so
// each guard's branch is tracked separately.
forgetest!(instrumented_multiple_modifiers, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    modifier nonZero(uint256 x) {
        require(x != 0, "zero");
        _;
    }

    modifier underCap(uint256 x) {
        require(x < 100, "too big");
        _;
    }

    function f(uint256 x) external nonZero(x) underCap(x) returns (uint256) {
        return x;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

interface Vm {
    function expectRevert(bytes calldata revertData) external;
}

contract TargetTest {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    Target t = new Target();

    function testAllGuardPaths() external {
        require(t.f(5) == 5);
        vm.expectRevert(bytes("zero"));
        t.f(0);
        vm.expectRevert(bytes("too big"));
        t.f(200);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testAllGuardPaths"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful with warnings:
Warning (2018): Function state mutability can be restricted to view
  [FILE]:15:5:
   |
15 |     function f(uint256 x) external nonZero(x) underCap(x) returns (uint256) {/// @solidity memory-safe-assembly
   |     ^ (Relevant source part starts here and spans across multiple lines).

Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testAllGuardPaths() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (6/6) | 100.00% (3/3) | 100.00% (4/4) | 100.00% (3/3) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (6/6) | 100.00% (3/3) | 100.00% (4/4) | 100.00% (3/3) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

// `do { .. } while` runs its body at least once; instrument it as a statement-bearing loop
// (not a branch) and confirm coverage with a single-statement body too.
forgetest!(instrumented_do_while_single_statement, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function countdown(uint256 n) external pure returns (uint256 steps) {
        do steps++; while (steps < n);
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testRuns() external view {
        require(t.countdown(3) == 3);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testRuns"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testRuns() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+------------+---------------╮
| File           | % Lines       | % Statements  | % Branches | % Funcs       |
+=============================================================================+
| src/Target.sol | 100.00% (2/2) | 100.00% (1/1) | N/A (0/0)  | 100.00% (1/1) |
|----------------+---------------+---------------+------------+---------------|
| Total          | 100.00% (2/2) | 100.00% (1/1) | N/A (0/0)  | 100.00% (1/1) |
╰----------------+---------------+---------------+------------+---------------╯

"#]]);
});

// Instrumented hits must propagate through the fuzz runner, not just unit tests. A fuzzed input
// drives both sides of a branch across runs, yielding full coverage.
forgetest!(instrumented_fuzz_coverage, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function classify(uint256 x) external pure returns (uint256) {
        if (x % 2 == 0) {
            return 0;
        }
        return 1;
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testFuzz_classify(uint256 x) external view {
        uint256 r = t.classify(x);
        require(r == 0 || r == 1);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testFuzz_classify"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testFuzz_classify(uint256) (runs: 256, [AVG_GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (4/4) | 100.00% (2/2) | 100.00% (2/2) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (4/4) | 100.00% (2/2) | 100.00% (2/2) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

// viaIR matrix: one contract exercising every instrumented construct (if/else-if, bare if,
// loops, ternary, ||, modifier guard, assembly, require), with a test that covers every
// reachable path. Coverage must be identical and fully accurate whether the optimizer/viaIR are
// off (default) or on (`--ir-minimum`), which is the property the instrumented mode exists to
// provide. This proves the source rewrites survive viaIR codegen for each construct, not just
// that they parse.
forgetest!(instrumented_viair_all_constructs, |prj, cmd| {
    prj.add_source(
        "AllConstructs.sol",
        r#"
contract AllConstructs {
    uint256 public state;

    modifier nonZero(uint256 x) {
        require(x != 0, "zero");
        _;
    }

    constructor(uint256 v) {
        state = v;
    }

    function ifElseIf(uint256 x) external pure returns (uint256) {
        if (x == 0) {
            return 1;
        } else if (x == 1) {
            return 2;
        }
        return 3;
    }

    function bareIf(uint256 x) external pure returns (uint256 r) {
        r = 10;
        if (x > 5) r = 20;
    }

    function loops(uint256 n) external pure returns (uint256 sum) {
        for (uint256 i = 0; i < n; i++) sum += i;
        while (sum < 100) sum += 1;
        do { sum += 1; } while (sum < 110);
    }

    function ternary(bool a) external pure returns (uint256) {
        return a ? 1 : 2;
    }

    function logical(bool a, bool b) external pure returns (bool) {
        return a || b;
    }

    function guarded(uint256 x) external nonZero(x) returns (uint256) {
        state = x;
        return x;
    }

    function asmFn(uint256 x) external pure returns (uint256 r) {
        assembly {
            r := add(x, 1)
        }
    }

    function req(uint256 x) external pure returns (uint256) {
        require(x > 0, "nonpositive");
        return x;
    }
}
"#,
    );
    prj.add_test(
        "AllConstructsTest.sol",
        r#"
import {AllConstructs} from "../src/AllConstructs.sol";

interface Vm {
    function expectRevert(bytes calldata revertData) external;
}

contract AllConstructsTest {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    AllConstructs c = new AllConstructs(1);

    function test_all() external {
        c.ifElseIf(0);
        c.ifElseIf(1);
        c.ifElseIf(2);
        c.bareIf(1);
        c.bareIf(10);
        c.loops(3);
        c.ternary(true);
        c.ternary(false);
        // Both `||` operands: left-true (short-circuits) and left-false then right.
        c.logical(true, false);
        c.logical(false, true);
        // Both modifier guard paths.
        c.guarded(5);
        vm.expectRevert(bytes("zero"));
        c.guarded(0);
        c.asmFn(1);
        // Both require paths.
        c.req(1);
        vm.expectRevert(bytes("nonpositive"));
        c.req(0);
    }
}
"#,
    );

    // Default mode: optimizer and viaIR disabled by coverage.
    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "test_all"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/AllConstructsTest.sol:AllConstructsTest
[PASS] test_all() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭-----------------------+-----------------+-----------------+-----------------+-----------------╮
| File                  | % Lines         | % Statements    | % Branches      | % Funcs         |
+===============================================================================================+
| src/AllConstructs.sol | 100.00% (29/29) | 100.00% (17/17) | 100.00% (14/14) | 100.00% (10/10) |
|-----------------------+-----------------+-----------------+-----------------+-----------------|
| Total                 | 100.00% (29/29) | 100.00% (17/17) | 100.00% (14/14) | 100.00% (10/10) |
╰-----------------------+-----------------+-----------------+-----------------+-----------------╯

"#]]);

    // `--ir-minimum`: the same sources compiled through viaIR must report identical coverage.
    cmd.forge_fuse()
        .arg("coverage")
        .args(["--instrumented", "--ir-minimum", "--exclude-tests", "--mt", "test_all"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/AllConstructsTest.sol:AllConstructsTest
[PASS] test_all() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭-----------------------+-----------------+-----------------+-----------------+-----------------╮
| File                  | % Lines         | % Statements    | % Branches      | % Funcs         |
+===============================================================================================+
| src/AllConstructs.sol | 100.00% (29/29) | 100.00% (17/17) | 100.00% (14/14) | 100.00% (10/10) |
|-----------------------+-----------------+-----------------+-----------------+-----------------|
| Total                 | 100.00% (29/29) | 100.00% (17/17) | 100.00% (14/14) | 100.00% (10/10) |
╰-----------------------+-----------------+-----------------+-----------------+-----------------╯

"#]]);
});

// Coverage is measured *inside* inline assembly for non-pure functions: each Yul statement and
// each Yul `if`/`switch` branch is counted. (A `pure` function cannot host the Yul probe's
// `staticcall`, so its assembly stays a single statement; that case is covered by the unit tests.)
forgetest!(instrumented_yul_assembly_coverage, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    uint256 public stored;

    function compute(uint256 x) external returns (uint256 r) {
        assembly {
            let a := add(x, 5)
            let b := mul(a, 2)
            if gt(x, 100) {
                b := add(b, 1)
            }
            r := add(a, b)
            sstore(0, r)
        }
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testBothYulPaths() external {
        require(t.compute(10) == 45);
        require(t.compute(200) == 616);
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testBothYulPaths"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/TargetTest.sol:TargetTest
[PASS] testBothYulPaths() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭----------------+---------------+---------------+---------------+---------------╮
| File           | % Lines       | % Statements  | % Branches    | % Funcs       |
+================================================================================+
| src/Target.sol | 100.00% (8/8) | 100.00% (6/6) | 100.00% (1/1) | 100.00% (1/1) |
|----------------+---------------+---------------+---------------+---------------|
| Total          | 100.00% (8/8) | 100.00% (6/6) | 100.00% (1/1) | 100.00% (1/1) |
╰----------------+---------------+---------------+---------------+---------------╯

"#]]);
});

// Adversarial: the coverage probe (a sentinel staticcall) must not disturb the contract's view
// of RETURNDATASIZE / RETURNDATACOPY. A probed statement is placed between an external call and a
// returndata read; the decoded values and returndatasize must be exactly what the real call left,
// including an empty buffer after a call to an account with no code.
forgetest!(instrumented_preserves_returndata, |prj, cmd| {
    prj.add_source(
        "RetData.sol",
        r#"
contract Returner {
    function get() external pure returns (uint256, uint256) {
        return (111, 222);
    }
}

contract RetData {
    Returner r = new Returner();
    uint256 public marker;

    function callThenDecode() external returns (uint256 a, uint256 b) {
        (bool ok, bytes memory data) = address(r).staticcall(abi.encodeWithSignature("get()"));
        require(ok, "call failed");
        marker = 7;
        (a, b) = abi.decode(data, (uint256, uint256));
    }

    function emptyAccountReturndata() external returns (uint256 size) {
        (bool ok,) = address(0xdead).staticcall("");
        require(ok);
        marker = 9;
        assembly {
            size := returndatasize()
        }
    }
}
"#,
    );
    prj.add_test(
        "RetDataTest.sol",
        r#"
import {RetData} from "../src/RetData.sol";

contract RetDataTest {
    RetData c = new RetData();

    function testReturndataIntact() external {
        (uint256 a, uint256 b) = c.callThenDecode();
        require(a == 111 && b == 222, "returndata corrupted by probe");
        require(c.emptyAccountReturndata() == 0, "stale returndata after empty-account call");
    }
}
"#,
    );

    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testReturndataIntact"])
        .assert_success()
        .stdout_eq(str![[r#"
[COMPILING_FILES] with [SOLC_VERSION]
[SOLC_VERSION] [ELAPSED]
Compiler run successful!
Analysing contracts...
Running tests...

Ran 1 test for test/RetDataTest.sol:RetDataTest
[PASS] testReturndataIntact() ([GAS])
Suite result: ok. 1 passed; 0 failed; 0 skipped; [ELAPSED]

Ran 1 test suite [ELAPSED]: 1 tests passed, 0 failed, 0 skipped (1 total tests)

╭-----------------+-----------------+-----------------+--------------+---------------╮
| File            | % Lines         | % Statements    | % Branches   | % Funcs       |
+====================================================================================+
| src/RetData.sol | 100.00% (13/13) | 100.00% (10/10) | 50.00% (2/4) | 100.00% (3/3) |
|-----------------+-----------------+-----------------+--------------+---------------|
| Total           | 100.00% (13/13) | 100.00% (10/10) | 50.00% (2/4) | 100.00% (3/3) |
╰-----------------+-----------------+-----------------+--------------+---------------╯

"#]]);
});

// Regression: a `require` used as the brace-less body of a control statement must compile under
// `--instrumented` (previously the synthesized braces detached and the file failed to build) and
// the guarded `require`'s line must not be double-counted in the LCOV `DA` magnitude.
forgetest!(instrumented_braceless_require_body, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function guardIf(bool a, uint256 x) external pure returns (uint256) {
        if (a) require(x > 0, "e");
        return x;
    }

    function guardLoop(uint256 n) external pure returns (uint256 i) {
        for (i = 0; i < n; i++) require(i < n, "e");
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";

contract TargetTest {
    Target t = new Target();

    function testGuards() external view {
        require(t.guardIf(true, 5) == 5);
        require(t.guardIf(false, 5) == 5);
        require(t.guardLoop(2) == 2);
    }
}
"#,
    );

    // Compiles and runs (the file used to fail to build); coverage is produced.
    cmd.arg("coverage")
        .args(["--instrumented", "--exclude-tests", "--mt", "testGuards", "--report=lcov"])
        .assert_success();

    // The `require` inside the loop (line 11) runs twice; its line must report 2 hits, not 4
    // (no double count from a redundant body statement probe).
    let lcov = prj.root().join("lcov.info");
    let content = std::fs::read_to_string(&lcov).unwrap();
    assert!(
        content.contains("DA:11,2"),
        "expected the loop-guarded require line to report 2 hits, got:\n{content}"
    );
    assert!(
        !content.contains("DA:11,4"),
        "loop-guarded require line was double-counted, got:\n{content}"
    );
});

forgetest!(instrumented_yul_local_names, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    function value() external view returns (uint256 result) {
        assembly {
            let _cov := 3
            let __foundry_coverage_scratch := 5
            result := add(_cov, __foundry_coverage_scratch)
        }
    }
}
"#,
    );
    prj.add_test(
        "TargetTest.sol",
        r#"
import {Target} from "../src/Target.sol";
contract TargetTest {
    function testValue() external {
        require(new Target().value() == 8);
    }
}
"#,
    );
    cmd.arg("coverage").args(["--instrumented", "--exclude-tests"]).assert_success();
});
