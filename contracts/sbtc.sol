// SPDX-License-Identifier: MIT

pragma solidity ^0.8.0;

library Sbtc {
    address constant private precompile = address(0x401);
    // input = 20 + 32 + 32 = 84 bytes
    // evm account transfer pcx to substrate account
    // @from is the current owner of pcx
    // @substrate is substrate account public key
    // @value is amount of balance
    function withdraw_pcx(
        address from,
        bytes32 substrate,
        uint256 value
    ) public returns (bool) {
        (bool success, bytes memory returnData) = precompile.call(abi.encodePacked(from, substrate, value));

        assembly {
            if eq(success, 0) {
                revert(add(returnData, 0x20), returndatasize())
            }
        }

        return abi.decode(returnData, (bool));
    }

    // input = 20 + 32 + 32 + 1 padding = 85 bytes
    // evm account transfer sbtc to substrate account
    // @from is the current owner of sbtc
    // @substrate is substrate account public key
    // @value is amount of balance
    function withdraw_sbtc(
        address from,
        bytes32 substrate,
        uint256 value
    ) public returns (bool) {
        bool padding = true;

        (bool success, bytes memory returnData) = precompile.call(abi.encodePacked(from, substrate, value, padding));

        assembly {
            if eq(success, 0) {
                revert(add(returnData, 0x20), returndatasize())
            }
        }

        return abi.decode(returnData, (bool));
    }

    // input = 20 + 20 + 32 = 72 bytes
    // evm account transfer sbtc to evm account
    // @from is the current owner of sbtc
    // @to is sbtc receiver
    // @value is amount of balance
    function transfer(
        address from,
        address to,
        uint256 value
    ) public returns (bool) {
        (bool success, bytes memory returnData) = precompile.call(abi.encodePacked(from, to, value));

        assembly {
            if eq(success, 0) {
                revert(add(returnData, 0x20), returndatasize())
            }
        }

        return abi.decode(returnData, (bool));
    }

    // input = 20 bytes
    // get sbtc free balance
    // @from is the current owner of sbtc
    function free_balance(
        address from
    ) internal view returns (uint256) {
        (bool success, bytes memory returnData) = precompile.staticcall(abi.encodePacked(from));

        assembly {
            if eq(success, 0) {
                revert(add(returnData, 0x20), returndatasize())
            }
        }

        return abi.decode(returnData, (uint256));
    }

    // input = 20 + 1 padding = 21 bytes
    // get sbtc total balance
    // @from is the current owner of sbtc
    function total_balance(
        address from
    ) internal view returns (uint256) {
        bool padding = true;

        (bool success, bytes memory returnData) = precompile.staticcall(abi.encodePacked(from, padding));

        assembly {
            if eq(success, 0) {
                revert(add(returnData, 0x20), returndatasize())
            }
        }

        return abi.decode(returnData, (uint256));
    }
}

contract Test {
    event WithdrawPcx(address indexed from, bytes32 indexed substrate, uint256 value);
    event WithdrawSbtc(address indexed from, bytes32 indexed substrate, uint256 value);
    event TransferSbtc(address indexed from, address indexed to, uint256 value);


    function t_withdraw_pcx(
        bytes32 substrate,
        uint256 value
    ) public {
        require(Sbtc.withdraw_pcx(msg.sender, substrate, value), "Invalid WithdrawPcx");

        emit WithdrawPcx(msg.sender, substrate, value);
    }

    function t_withdraw_sbtc(
        bytes32 substrate,
        uint256 value
    ) public {
        require(Sbtc.withdraw_sbtc(msg.sender, substrate, value), "Invalid WithdrawSbtc");

        emit WithdrawSbtc(msg.sender, substrate, value);
    }

    function t_transfer_sbtc(
        address to,
        uint256 value
    ) public {
        require(Sbtc.transfer(msg.sender, to, value), "Invalid TransferSbtc");

        emit TransferSbtc(msg.sender, to, value);
    }

    function t_free_balance(
        address from
    ) public view returns (uint256) {
        return Sbtc.free_balance(from);
    }

    function t_total_balance(
        address from
    ) public view returns (uint256) {
        return Sbtc.total_balance(from);
    }
}
